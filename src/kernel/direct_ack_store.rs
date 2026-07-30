// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199D — the **bounded, endpoint-indexed, generation-bearing multi-pair**
//! blocked-waiter acknowledgement store.
//!
//! This replaces the Stage 199A2B3 single-outstanding-pair slot that
//! [`crate::kernel::boot::ipccall_direct_ack`] and
//! [`crate::kernel::boot::ipcreply_direct_ack`] used to be. That slot held exactly ONE
//! committed acknowledgement: a second simultaneous pair would overwrite (or steal) the
//! active unclaimed acknowledgement, so it was classified ORACLE-ONLY and defended only by
//! a fail-closed overwrite fuse. The Stage 199A2D1 race model specifies the replacement
//! implemented here.
//!
//! # Model
//!
//! The store is a fixed array of [`DIRECT_ACK_STORE_CAPACITY`] slots. Nothing is allocated
//! and nothing is unbounded: capacity exhaustion is a first-class, *refusable* outcome that
//! is reported **before** any irreversible publication occurs.
//!
//! Each slot runs a linear lifecycle:
//!
//! ```text
//!   Vacant ──reserve──▶ Reserved ──commit──▶ Committed ──consume──▶ Consumed
//!      ▲                   │                     │                     │
//!      └──────cancel───────┘                     └───restore◀──────────┘
//!      └──────────────────── reset / release ────────────────────┘
//! ```
//!
//! * **reserve** takes exclusive ownership of a free slot for one
//!   `(endpoint_index, endpoint_generation)` pair and one waiter incarnation
//!   `{tid, asid}`. It publishes NOTHING an observer can consume — a reservation is
//!   invisible to [`DirectAckStore::consume`]. Reserve is the ONLY point at which capacity
//!   can be refused, and it refuses before the caller has performed any irreversible work.
//! * **commit** is the single irreversible publication. It consumes the
//!   [`AckReservation`] token by value (so a reservation can be committed at most once) and
//!   requires the committed fields to name exactly the endpoint and waiter incarnation the
//!   reservation was taken for.
//! * **consume** is the single ownership transfer. It is keyed by the EXACT
//!   `(endpoint_index, endpoint_generation)` pair, optionally by the exact waiter
//!   incarnation, and transitions `Committed → Consumed` with one compare-exchange, so a
//!   duplicate trap/drain cannot consume the same acknowledgement twice.
//! * **cancel** returns an uncommitted reservation to `Vacant`. Because a reservation is
//!   never observable, a cancelled pair leaves no slot occupied and no waiter identity
//!   readable — the rollback is leak-free by construction.
//! * **restore** re-arms a consumed acknowledgement for a retryable rollback of the SAME
//!   publication (matching `seq`). A publication that has been superseded cannot be
//!   restored.
//!
//! # Identity and staleness
//!
//! Every slot carries a monotonically increasing per-slot `slot_generation`, bumped on each
//! entry into `Reserved`. An [`AckReservation`] names `(slot, slot_generation)`, so a token
//! that outlived its slot (because the store was reset, or the slot was released and
//! re-reserved) can neither commit nor cancel someone else's slot.
//!
//! Consumption is rejected — and separately counted — when the endpoint index is unknown,
//! the endpoint generation differs (a stale incarnation of a recycled endpoint slot), the
//! waiter incarnation differs (a foreign or recycled task), the pair is still only
//! reserved, or the pair was already consumed. Every rejection path is fail-closed: it
//! mutates nothing and yields no acknowledgement.
//!
//! # Concurrency
//!
//! All state is per-slot atomics. Publication is a Release store of the slot state after
//! the fields; consumption is an AcqRel compare-exchange, so a consumer that wins the CAS
//! sees every field written before the commit. Slot acquisition is a `Vacant → Reserved`
//! compare-exchange, which is what makes two CPUs racing for the last free slot resolve to
//! exactly one winner and one capacity refusal.
//!
//! Reservation additionally takes a **leaf admission guard**, because the endpoint-uniqueness
//! check and the slot allocation live on different words and must be ONE decision: without it
//! two CPUs reserving the same endpoint could each observe "no live pair here" and then each
//! win a different vacant slot, leaving one endpoint index owning two slots. The guard is a
//! leaf — its critical section takes no other lock, allocates nothing, calls nothing fallible,
//! and is bounded by one pass over the fixed slot array. Commit, consume, cancel, restore and
//! every reader stay entirely lock-free.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64, AtomicUsize, Ordering};

/// Number of simultaneously outstanding blocked-waiter pairs one store can hold.
///
/// Bounded and fixed at compile time: the store performs no allocation, so this is also the
/// exact point at which [`DirectAckStore::reserve`] starts refusing.
pub const DIRECT_ACK_STORE_CAPACITY: usize = 8;

const SLOT_VACANT: u8 = 0;
const SLOT_RESERVED: u8 = 1;
const SLOT_COMMITTED: u8 = 2;
const SLOT_CONSUMED: u8 = 3;
/// Terminal, spent, and **not** consumed: the waiter this pair was published for left through
/// a non-direct edge (legacy delivery, timeout, cancellation, death, endpoint destruction,
/// stale cleanup), so the lease is over and the acknowledgement was never handed out.
///
/// A distinct terminal state rather than a return to `Vacant`, for the same reason
/// `Consumed` is: a spent slot must stay *findable* so a duplicate release is
/// distinguishable from an absent pair, and so [`DirectAckStore::consume`] can refuse a
/// pair whose lease has already ended instead of silently delivering into a departed
/// waiter's buffers.
const SLOT_RELEASED: u8 = 4;

/// Sentinel stored in a slot's endpoint index while the slot holds no pair.
const NO_ENDPOINT: usize = usize::MAX;

/// Exact identity of the endpoint a pair is bound to.
///
/// Both halves are load-bearing: the index selects the endpoint slot, the generation pins
/// the endpoint *incarnation*, so an acknowledgement published for a recycled endpoint slot
/// can never be consumed by a later incarnation of the same index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AckEndpoint {
    pub(crate) index: usize,
    pub(crate) generation: u64,
}

impl AckEndpoint {
    pub(crate) const fn new(index: usize, generation: u64) -> Self {
        Self { index, generation }
    }
}

/// Exact incarnation of the blocked waiter an acknowledgement belongs to (the server for a
/// blocked-server ack, the caller for a blocked-caller ack).
///
/// The address-space id is part of the identity, so a recycled thread id belonging to a
/// different address space is a DIFFERENT incarnation and cannot commit into, or consume,
/// the earlier incarnation's pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AckWaiter {
    pub(crate) tid: u64,
    pub(crate) asid: u16,
}

impl AckWaiter {
    pub(crate) const fn new(tid: u64, asid: u16) -> Self {
        Self { tid, asid }
    }
}

/// The committed acknowledgement payload: who is blocked, on which endpoint incarnation,
/// and where their userspace destinations are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AckFields {
    pub(crate) endpoint: AckEndpoint,
    pub(crate) waiter: AckWaiter,
    pub(crate) payload_user_ptr: usize,
    pub(crate) payload_user_len: usize,
    pub(crate) meta_user_ptr: usize,
    pub(crate) meta_user_len: usize,
}

/// Exclusive ownership of one reserved-but-not-yet-published slot.
///
/// Deliberately NOT `Copy`/`Clone`: [`DirectAckStore::commit`] and
/// [`DirectAckStore::cancel`] both take it by value, so a reservation resolves exactly once
/// — it cannot be committed twice, nor committed and then cancelled.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a reservation must be committed or cancelled, or the slot leaks"]
pub(crate) struct AckReservation {
    slot: usize,
    slot_generation: u64,
    endpoint: AckEndpoint,
    waiter: AckWaiter,
}

impl AckReservation {
    /// The endpoint incarnation this reservation was taken for.
    #[allow(dead_code)]
    pub(crate) fn endpoint(&self) -> AckEndpoint {
        self.endpoint
    }

    /// The waiter incarnation this reservation was taken for.
    #[allow(dead_code)]
    pub(crate) fn waiter(&self) -> AckWaiter {
        self.waiter
    }

    /// Index of the reserved slot (diagnostics and tests only).
    #[allow(dead_code)]
    pub(crate) fn slot(&self) -> usize {
        self.slot
    }
}

/// Why [`DirectAckStore::reserve`] refused.
///
/// Both variants are refusals *before* any irreversible publication, so the caller can
/// abandon the pair without unwinding anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AckReserveError {
    /// Every slot is occupied by a live (reserved or committed) pair.
    CapacityExhausted,
    /// The endpoint incarnation already owns a live pair. Overwriting it would destroy an
    /// acknowledgement that has been published and not yet consumed — the fail-closed
    /// successor of the single-slot overwrite fuse.
    EndpointAlreadyLive,
}

/// Why [`DirectAckStore::commit`] refused. The reservation is handed back so the caller can
/// cancel it and leave no slot occupied.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AckCommitError {
    /// The committed fields name a different endpoint incarnation than the reservation.
    EndpointMismatch(AckReservation),
    /// The committed fields name a different waiter incarnation than the reservation.
    WaiterMismatch(AckReservation),
    /// The slot no longer belongs to this reservation (reset or released underneath it).
    Stale(AckReservation),
}

impl AckCommitError {
    /// Recover the reservation so it can be cancelled.
    pub(crate) fn into_reservation(self) -> AckReservation {
        match self {
            Self::EndpointMismatch(r) | Self::WaiterMismatch(r) | Self::Stale(r) => r,
        }
    }
}

/// The outcome of an attempted consumption. Every non-`Consumed` variant is fail-closed: no
/// acknowledgement is handed out and no slot state changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AckConsume {
    /// Exactly-once ownership transfer: the acknowledgement and its publication sequence.
    Consumed(AckFields, u64),
    /// No slot holds a pair for this endpoint index.
    Absent,
    /// A pair exists for this endpoint index, but for a different endpoint generation.
    StaleGeneration,
    /// The pair matches the endpoint exactly but names a different waiter incarnation.
    ForeignWaiter,
    /// The pair is reserved but not yet published.
    NotCommitted,
    /// The pair was already consumed — a duplicate trap/drain.
    AlreadyConsumed,
    /// The lease already ended through a NON-direct edge: the waiter this acknowledgement
    /// was published for has been removed (legacy delivery, timeout, cancellation, death,
    /// endpoint destruction, stale cleanup). This is the mutual-exclusion refusal — the two
    /// terminal edges can never both resolve one pair.
    AlreadyReleased,
}

impl AckConsume {
    /// The acknowledgement and sequence on success; `None` on every rejection.
    #[allow(dead_code)]
    pub(crate) fn ok(self) -> Option<(AckFields, u64)> {
        match self {
            Self::Consumed(fields, seq) => Some((fields, seq)),
            _ => None,
        }
    }
}

/// The outcome of an attempted **lease release** — the non-direct terminal edge, taken when
/// the endpoint receive-waiter a pair was published for is removed for any reason other than
/// a direct delivery.
///
/// Only [`AckRelease::Released`] mutates. Every other variant leaves the slot exactly as it
/// was, so a foreign, stale or repeated release is a pure no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AckRelease {
    /// The exact committed pair was live and its lease is now over. Carries the publication
    /// sequence that was retired. The only mutating result, and it happens at most once.
    Released(u64),
    /// No slot holds a pair for this endpoint index. **The overwhelmingly common case** —
    /// most endpoint waiters never had an acknowledgement published — and a pure no-op, so
    /// it is deliberately not counted.
    Absent,
    /// A pair exists for this endpoint index, but for a different endpoint *incarnation*.
    /// Releasing it would retire a lease belonging to a later endpoint generation.
    StaleGeneration,
    /// The pair matches the endpoint exactly but names a different waiter incarnation — a
    /// replacement task that reused the numeric TID with a fresh ASID is not our waiter.
    ForeignWaiter,
    /// The pair is reserved but not yet published: the publisher still owns it and will
    /// commit or cancel. Reachable only when a removal interleaves a publisher's
    /// reserve → commit window, which cannot happen on a uniprocessor boot.
    NotCommitted,
    /// The direct edge already consumed this pair. **Expected and benign**: it is exactly
    /// what a successful direct delivery leaves behind, since the transaction consumes the
    /// acknowledgement and then removes the waiter it delivered to.
    AlreadyConsumed,
    /// This lease was already released — a duplicate non-direct terminal.
    AlreadyReleased,
}

impl AckRelease {
    /// The retired publication sequence, when this call is the one that ended the lease.
    pub(crate) fn released_seq(self) -> Option<u64> {
        match self {
            Self::Released(seq) => Some(seq),
            Self::Absent
            | Self::StaleGeneration
            | Self::ForeignWaiter
            | Self::NotCommitted
            | Self::AlreadyConsumed
            | Self::AlreadyReleased => None,
        }
    }

    /// True for the outcomes that indicate a real accounting anomaly, as opposed to the
    /// ordinary no-ops (`Absent`, `AlreadyConsumed`) and the SMP-only publisher race
    /// (`NotCommitted`). These are the ones a quiescent attestation requires to be zero.
    pub(crate) fn is_anomaly(self) -> bool {
        matches!(
            self,
            Self::StaleGeneration | Self::ForeignWaiter | Self::AlreadyReleased
        )
    }
}

/// One acknowledgement slot. Every field is an independent atomic; the `state` field is the
/// publication barrier (Release on commit, AcqRel on consume).
#[derive(Debug)]
struct AckSlot {
    state: AtomicU8,
    /// Bumped on every entry into `Reserved`, so a stale [`AckReservation`] is detectable.
    slot_generation: AtomicU64,
    /// Store-wide monotonic publication sequence, assigned at commit.
    seq: AtomicU64,
    endpoint_index: AtomicUsize,
    endpoint_generation: AtomicU64,
    waiter_tid: AtomicU64,
    waiter_asid: AtomicU16,
    payload_user_ptr: AtomicUsize,
    payload_user_len: AtomicUsize,
    meta_user_ptr: AtomicUsize,
    meta_user_len: AtomicUsize,
}

impl AckSlot {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(SLOT_VACANT),
            slot_generation: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            endpoint_index: AtomicUsize::new(NO_ENDPOINT),
            endpoint_generation: AtomicU64::new(0),
            waiter_tid: AtomicU64::new(0),
            waiter_asid: AtomicU16::new(0),
            payload_user_ptr: AtomicUsize::new(0),
            payload_user_len: AtomicUsize::new(0),
            meta_user_ptr: AtomicUsize::new(0),
            meta_user_len: AtomicUsize::new(0),
        }
    }

    /// Wipe every identity/destination field. Called on the paths that return a slot to
    /// `Vacant`, so no waiter identity or userspace pointer survives a rollback.
    fn clear_fields(&self) {
        self.endpoint_index.store(NO_ENDPOINT, Ordering::Relaxed);
        self.endpoint_generation.store(0, Ordering::Relaxed);
        self.waiter_tid.store(0, Ordering::Relaxed);
        self.waiter_asid.store(0, Ordering::Relaxed);
        self.payload_user_ptr.store(0, Ordering::Relaxed);
        self.payload_user_len.store(0, Ordering::Relaxed);
        self.meta_user_ptr.store(0, Ordering::Relaxed);
        self.meta_user_len.store(0, Ordering::Relaxed);
        self.seq.store(0, Ordering::Relaxed);
    }

    fn endpoint(&self) -> AckEndpoint {
        AckEndpoint::new(
            self.endpoint_index.load(Ordering::Relaxed),
            self.endpoint_generation.load(Ordering::Relaxed),
        )
    }

    fn waiter(&self) -> AckWaiter {
        AckWaiter::new(
            self.waiter_tid.load(Ordering::Relaxed),
            self.waiter_asid.load(Ordering::Relaxed),
        )
    }

    fn fields(&self) -> AckFields {
        AckFields {
            endpoint: self.endpoint(),
            waiter: self.waiter(),
            payload_user_ptr: self.payload_user_ptr.load(Ordering::Relaxed),
            payload_user_len: self.payload_user_len.load(Ordering::Relaxed),
            meta_user_ptr: self.meta_user_ptr.load(Ordering::Relaxed),
            meta_user_len: self.meta_user_len.load(Ordering::Relaxed),
        }
    }

    /// True while the slot holds a pair that is not yet consumed (reserved or committed).
    fn is_live(&self) -> bool {
        matches!(
            self.state.load(Ordering::Acquire),
            SLOT_RESERVED | SLOT_COMMITTED
        )
    }
}

/// The bounded, endpoint-indexed, generation-bearing multi-pair acknowledgement store.
#[derive(Debug)]
pub(crate) struct DirectAckStore {
    slots: [AckSlot; DIRECT_ACK_STORE_CAPACITY],
    /// Leaf admission guard, held ONLY across the reservation's slot-allocation and
    /// endpoint-uniqueness decision.
    ///
    /// It exists because those two steps must be ONE atomic decision: without it, two CPUs
    /// reserving the same endpoint can each observe "no live pair here" and then each win a
    /// different vacant slot, leaving one endpoint index owning two slots. No CAS ordering
    /// fixes that on its own — the check and the allocation are on different words.
    ///
    /// It is a leaf: the critical section takes no other lock, allocates nothing, calls
    /// nothing fallible, and is bounded by one pass over the fixed slot array. Every other
    /// operation — commit, consume, cancel, restore, snapshot — stays entirely lock-free.
    admission: AtomicBool,
    next_seq: AtomicU64,
    reserves: AtomicU64,
    /// Highest simultaneous LIVE-pair occupancy observed this boot. Never exceeds
    /// [`DIRECT_ACK_STORE_CAPACITY`] by construction: it is sampled under the admission
    /// guard immediately after a reservation is stamped.
    occupancy_high_watermark: AtomicUsize,
    capacity_refusals: AtomicU64,
    endpoint_live_refusals: AtomicU64,
    stale_generation_rejections: AtomicU64,
    foreign_waiter_rejections: AtomicU64,
    not_committed_rejections: AtomicU64,
    duplicate_consume_rejections: AtomicU64,
    duplicate_release_rejections: AtomicU64,
    /// Consumption refused because the lease had already ended through a non-direct edge.
    /// The mutual-exclusion counter: it is the direct edge arriving after the waiter left.
    crossed_terminal_rejections: AtomicU64,
    commits: AtomicU64,
    consumes: AtomicU64,
    /// Leases retired through the non-direct terminal edge. The fourth resolution alongside
    /// consume and cancel: `reserves == consumes + releases + cancels` at quiescence.
    releases: AtomicU64,
    /// Releases that found the pair already consumed by the direct edge. Not an anomaly —
    /// one is produced by every successful direct delivery — but tracked, because it is the
    /// bridge term that makes the reserve/terminal balance legible.
    releases_after_consume: AtomicU64,
    cancels: AtomicU64,
}

impl DirectAckStore {
    pub(crate) const fn new() -> Self {
        Self {
            slots: [const { AckSlot::new() }; DIRECT_ACK_STORE_CAPACITY],
            admission: AtomicBool::new(false),
            next_seq: AtomicU64::new(1),
            reserves: AtomicU64::new(0),
            occupancy_high_watermark: AtomicUsize::new(0),
            capacity_refusals: AtomicU64::new(0),
            endpoint_live_refusals: AtomicU64::new(0),
            stale_generation_rejections: AtomicU64::new(0),
            foreign_waiter_rejections: AtomicU64::new(0),
            not_committed_rejections: AtomicU64::new(0),
            duplicate_consume_rejections: AtomicU64::new(0),
            duplicate_release_rejections: AtomicU64::new(0),
            crossed_terminal_rejections: AtomicU64::new(0),
            commits: AtomicU64::new(0),
            consumes: AtomicU64::new(0),
            releases: AtomicU64::new(0),
            releases_after_consume: AtomicU64::new(0),
            cancels: AtomicU64::new(0),
        }
    }

    /// Return every slot to `Vacant`, wipe all identities, and zero every counter.
    ///
    /// Test setup / between-boot reset only. Bumping each slot generation invalidates any
    /// outstanding [`AckReservation`], so a token that survives a reset can neither commit
    /// nor cancel into the fresh store.
    pub(crate) fn reset(&self) {
        for slot in &self.slots {
            slot.slot_generation.fetch_add(1, Ordering::Relaxed);
            slot.clear_fields();
            slot.state.store(SLOT_VACANT, Ordering::Release);
        }
        self.reserves.store(0, Ordering::Relaxed);
        self.occupancy_high_watermark.store(0, Ordering::Relaxed);
        self.capacity_refusals.store(0, Ordering::Relaxed);
        self.endpoint_live_refusals.store(0, Ordering::Relaxed);
        self.stale_generation_rejections.store(0, Ordering::Relaxed);
        self.foreign_waiter_rejections.store(0, Ordering::Relaxed);
        self.not_committed_rejections.store(0, Ordering::Relaxed);
        self.duplicate_consume_rejections
            .store(0, Ordering::Relaxed);
        self.duplicate_release_rejections
            .store(0, Ordering::Relaxed);
        self.crossed_terminal_rejections.store(0, Ordering::Relaxed);
        self.commits.store(0, Ordering::Relaxed);
        self.consumes.store(0, Ordering::Relaxed);
        self.releases.store(0, Ordering::Relaxed);
        self.releases_after_consume.store(0, Ordering::Relaxed);
        self.cancels.store(0, Ordering::Relaxed);
    }

    fn next_seq(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Index of the slot holding a pair for this endpoint index in ANY non-vacant state
    /// (reserved, committed or consumed), if any.
    ///
    /// At most one slot can hold a given endpoint index at a time: a live pair blocks a
    /// second reservation ([`AckReserveError::EndpointAlreadyLive`]), and a spent
    /// (consumed) pair is recycled by [`Self::reserve`] rather than duplicated. A consumed
    /// pair is deliberately still findable — that is what makes a duplicate consumption
    /// distinguishable from an absent one, and what keeps [`Self::restore`] able to re-arm
    /// the retryable rollback of the same publication.
    fn find_slot_for_index(&self, endpoint_index: usize) -> Option<usize> {
        self.slots.iter().position(|slot| {
            slot.state.load(Ordering::Acquire) != SLOT_VACANT
                && slot.endpoint_index.load(Ordering::Relaxed) == endpoint_index
        })
    }

    /// Take exclusive ownership of a slot to reserve into: a vacant slot if one exists,
    /// otherwise the least recently published SPENT slot — one whose lease has already
    /// reached a terminal edge, either `Consumed` (the acknowledgement transferred ownership)
    /// or `Released` (the waiter left and it never will). Neither can be claimed again, so
    /// both are safe to recycle.
    ///
    /// Returns `None` only when every slot holds a LIVE pair — the true capacity bound on
    /// simultaneously outstanding pairs.
    fn acquire_slot(&self) -> Option<usize> {
        for (index, slot) in self.slots.iter().enumerate() {
            if slot
                .state
                .compare_exchange(
                    SLOT_VACANT,
                    SLOT_RESERVED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return Some(index);
            }
        }
        // No vacant slot: recycle the spent pair with the lowest publication sequence.
        loop {
            let mut victim: Option<(usize, u64, u8)> = None;
            for (index, slot) in self.slots.iter().enumerate() {
                let state = slot.state.load(Ordering::Acquire);
                if state != SLOT_CONSUMED && state != SLOT_RELEASED {
                    continue;
                }
                let seq = slot.seq.load(Ordering::Relaxed);
                if victim.is_none_or(|(_, best, _)| seq < best) {
                    victim = Some((index, seq, state));
                }
            }
            let (index, _, spent_state) = victim?;
            if self.slots[index]
                .state
                .compare_exchange(
                    spent_state,
                    SLOT_RESERVED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                // Drop the spent pair's identity immediately, so a concurrent lock-free
                // reader never sees the recycled slot still advertising the OLD endpoint.
                self.slots[index].clear_fields();
                return Some(index);
            }
            // Lost the race for that victim; re-scan.
        }
    }

    /// Acquire the leaf admission guard, spinning until it is free.
    ///
    /// Never nests (the critical section takes no other lock) and never spans a fallible
    /// call, so it cannot deadlock or be held across a fault.
    fn lock_admission(&self) {
        while self
            .admission
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    fn unlock_admission(&self) {
        self.admission.store(false, Ordering::Release);
    }

    /// Reserve one slot for `(endpoint, waiter)`.
    ///
    /// This is the ONLY refusal point, and it happens before any irreversible publication:
    /// on `Err` the caller has published nothing and has nothing to unwind. A successful
    /// reservation is invisible to consumers until [`Self::commit`].
    ///
    /// The endpoint-uniqueness decision and the slot allocation are taken together under the
    /// leaf admission guard, so two CPUs reserving the SAME endpoint resolve to exactly one
    /// winner and one `EndpointAlreadyLive` refusal — never to two slots owning one endpoint.
    pub(crate) fn reserve(
        &self,
        endpoint: AckEndpoint,
        waiter: AckWaiter,
    ) -> Result<AckReservation, AckReserveError> {
        self.lock_admission();
        let outcome = self.reserve_admitted(endpoint, waiter);
        self.unlock_admission();
        outcome
    }

    /// The reservation decision, taken under the admission guard.
    fn reserve_admitted(
        &self,
        endpoint: AckEndpoint,
        waiter: AckWaiter,
    ) -> Result<AckReservation, AckReserveError> {
        // Refuse a second live pair for the same endpoint index BEFORE occupying a slot.
        // This is the fail-closed successor of the single-slot overwrite fuse: the already
        // published, not-yet-consumed acknowledgement is preserved untouched.
        if let Some(existing) = self.find_slot_for_index(endpoint.index) {
            if self.slots[existing].is_live() {
                self.endpoint_live_refusals.fetch_add(1, Ordering::Relaxed);
                return Err(AckReserveError::EndpointAlreadyLive);
            }
            // A SPENT pair for this endpoint index: recycle its slot so one endpoint index
            // never occupies two slots at once.
            self.release_slot(existing);
        }
        let Some(index) = self.acquire_slot() else {
            self.capacity_refusals.fetch_add(1, Ordering::Relaxed);
            return Err(AckReserveError::CapacityExhausted);
        };
        {
            let slot = &self.slots[index];
            // Won the slot: stamp the identity the reservation is bound to. The pair is
            // still invisible to `consume` (state is Reserved, not Committed).
            let slot_generation = slot.slot_generation.fetch_add(1, Ordering::Relaxed) + 1;
            slot.seq.store(0, Ordering::Relaxed);
            slot.endpoint_index.store(endpoint.index, Ordering::Relaxed);
            slot.endpoint_generation
                .store(endpoint.generation, Ordering::Relaxed);
            slot.waiter_tid.store(waiter.tid, Ordering::Relaxed);
            slot.waiter_asid.store(waiter.asid, Ordering::Relaxed);
            slot.payload_user_ptr.store(0, Ordering::Relaxed);
            slot.payload_user_len.store(0, Ordering::Relaxed);
            slot.meta_user_ptr.store(0, Ordering::Relaxed);
            slot.meta_user_len.store(0, Ordering::Relaxed);
            self.reserves.fetch_add(1, Ordering::Relaxed);
            // Sample occupancy under the admission guard, so the watermark is a real
            // simultaneous-pair count and can never exceed the capacity.
            let live = self.live_pair_count();
            self.occupancy_high_watermark
                .fetch_max(live, Ordering::Relaxed);
            Ok(AckReservation {
                slot: index,
                slot_generation,
                endpoint,
                waiter,
            })
        }
    }

    /// True iff the reservation still owns its slot.
    fn reservation_owns(&self, reservation: &AckReservation) -> bool {
        let slot = &self.slots[reservation.slot];
        slot.state.load(Ordering::Acquire) == SLOT_RESERVED
            && slot.slot_generation.load(Ordering::Relaxed) == reservation.slot_generation
    }

    /// Publish the acknowledgement — the single irreversible step.
    ///
    /// Consumes the reservation, so one reservation commits at most once. The fields must
    /// name exactly the endpoint incarnation and waiter incarnation the reservation was
    /// taken for; anything else is refused and the reservation is handed back for
    /// cancellation. Returns the publication sequence.
    pub(crate) fn commit(
        &self,
        reservation: AckReservation,
        fields: AckFields,
    ) -> Result<u64, AckCommitError> {
        if fields.endpoint != reservation.endpoint {
            return Err(AckCommitError::EndpointMismatch(reservation));
        }
        if fields.waiter != reservation.waiter {
            return Err(AckCommitError::WaiterMismatch(reservation));
        }
        if !self.reservation_owns(&reservation) {
            return Err(AckCommitError::Stale(reservation));
        }
        let slot = &self.slots[reservation.slot];
        let seq = self.next_seq();
        slot.payload_user_ptr
            .store(fields.payload_user_ptr, Ordering::Relaxed);
        slot.payload_user_len
            .store(fields.payload_user_len, Ordering::Relaxed);
        slot.meta_user_ptr
            .store(fields.meta_user_ptr, Ordering::Relaxed);
        slot.meta_user_len
            .store(fields.meta_user_len, Ordering::Relaxed);
        slot.seq.store(seq, Ordering::Relaxed);
        // Release: every field above is visible to any consumer that acquires this state.
        slot.state.store(SLOT_COMMITTED, Ordering::Release);
        self.commits.fetch_add(1, Ordering::Relaxed);
        Ok(seq)
    }

    /// Abandon an uncommitted reservation.
    ///
    /// Returns the slot to `Vacant` with every identity and destination wiped. Because a
    /// reservation was never observable, this leaves no slot occupied and no waiter
    /// identity readable — the rollback is leak-free. Returns `false` for a stale token
    /// (the slot already moved on), which changes nothing.
    pub(crate) fn cancel(&self, reservation: AckReservation) -> bool {
        if !self.reservation_owns(&reservation) {
            return false;
        }
        let slot = &self.slots[reservation.slot];
        slot.clear_fields();
        slot.state.store(SLOT_VACANT, Ordering::Release);
        self.cancels.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Consume the acknowledgement published for EXACTLY this endpoint incarnation, at most
    /// once.
    ///
    /// When `expect_waiter` is `Some`, the stored waiter incarnation must match exactly;
    /// this is the caller/server incarnation check for consumers that already know whose
    /// acknowledgement they are entitled to. Every rejection is classified and counted, and
    /// mutates no slot state.
    pub(crate) fn consume(
        &self,
        endpoint: AckEndpoint,
        expect_waiter: Option<AckWaiter>,
    ) -> AckConsume {
        let Some(index) = self.find_slot_for_index(endpoint.index) else {
            return AckConsume::Absent;
        };
        let slot = &self.slots[index];
        if slot.endpoint_generation.load(Ordering::Relaxed) != endpoint.generation {
            self.stale_generation_rejections
                .fetch_add(1, Ordering::Relaxed);
            return AckConsume::StaleGeneration;
        }
        // MUTUAL EXCLUSION, checked before the waiter compare: a released slot has had its
        // waiter identity wiped, so comparing identities first would misreport an ended lease
        // as a foreign waiter. The two terminal edges can never both resolve one pair.
        if slot.state.load(Ordering::Acquire) == SLOT_RELEASED {
            self.crossed_terminal_rejections
                .fetch_add(1, Ordering::Relaxed);
            return AckConsume::AlreadyReleased;
        }
        if let Some(expected) = expect_waiter
            && slot.waiter() != expected
        {
            self.foreign_waiter_rejections
                .fetch_add(1, Ordering::Relaxed);
            return AckConsume::ForeignWaiter;
        }
        match slot.state.load(Ordering::Acquire) {
            SLOT_RESERVED => {
                self.not_committed_rejections
                    .fetch_add(1, Ordering::Relaxed);
                return AckConsume::NotCommitted;
            }
            SLOT_CONSUMED => {
                self.duplicate_consume_rejections
                    .fetch_add(1, Ordering::Relaxed);
                return AckConsume::AlreadyConsumed;
            }
            SLOT_COMMITTED => {}
            // `SLOT_RELEASED` is already refused above; `SLOT_VACANT` cannot be found.
            _ => return AckConsume::Absent,
        }
        // Exactly-once ownership transfer.
        if slot
            .state
            .compare_exchange(
                SLOT_COMMITTED,
                SLOT_CONSUMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            self.duplicate_consume_rejections
                .fetch_add(1, Ordering::Relaxed);
            return AckConsume::AlreadyConsumed;
        }
        self.consumes.fetch_add(1, Ordering::Relaxed);
        AckConsume::Consumed(slot.fields(), slot.seq.load(Ordering::Relaxed))
    }

    /// End the lease on the acknowledgement published for EXACTLY this endpoint incarnation
    /// and waiter incarnation — the **non-direct terminal edge**, at most once.
    ///
    /// This is what makes the lease owned by the endpoint waiter's lifecycle rather than by
    /// the direct delivery path. A pair is published when a task commits a blocking recv-v2;
    /// before this edge existed the only way to retire one was a direct delivery's
    /// [`Self::consume`], so every waiter satisfied by the legacy path, a timeout, a
    /// cancellation, task or server death, endpoint destruction, or stale cleanup orphaned a
    /// committed slot **permanently**. Publication was driven by blocking and consumption by
    /// delivery, and the two were never paired.
    ///
    /// Identity is exact in all four components: endpoint index, endpoint generation, waiter
    /// TID and waiter ASID. A stale generation or a foreign waiter mutates nothing, so a
    /// replacement task that reused a numeric TID can never retire its predecessor's lease,
    /// and a recycled endpoint slot can never retire the previous incarnation's.
    /// # Why this takes the admission guard
    ///
    /// Release is the one operation that must be excluded against slot **recycling**. A
    /// consumer racing a release cannot corrupt anything — `consume` and `release` both
    /// compare-exchange out of `Committed`, so exactly one wins. But a *recycle* can: if a
    /// release observes slot `i` as `Committed` for its endpoint, and slot `i` is then
    /// consumed, re-reserved and re-committed for a fresh pair before the release's own
    /// compare-exchange runs, that exchange would retire a lease it never inspected.
    ///
    /// `acquire_slot` — the only recycler — takes this guard, so holding it here makes
    /// "find the slot, verify its identity, retire it" one decision, exactly as reservation
    /// makes "check uniqueness, allocate" one decision. The guard is a leaf: taking it while
    /// holding the IPC state lock is safe because its critical section takes no other lock.
    ///
    /// The overwhelmingly common case — a waiter removal on an endpoint that never published —
    /// is resolved **lock-free before the guard is taken**, so ordinary removals pay nothing.
    pub(crate) fn release(&self, endpoint: AckEndpoint, waiter: AckWaiter) -> AckRelease {
        if self.find_slot_for_index(endpoint.index).is_none() {
            // No pair for this endpoint: the ordinary case for the vast majority of waiter
            // removals. Deliberately uncounted — this is on every endpoint waiter removal.
            return AckRelease::Absent;
        }
        self.lock_admission();
        let outcome = self.release_locked(endpoint, waiter);
        self.unlock_admission();
        outcome
    }

    /// The release decision. Callers hold the admission guard, so no slot can be recycled
    /// between the identity checks and the state transition.
    fn release_locked(&self, endpoint: AckEndpoint, waiter: AckWaiter) -> AckRelease {
        let Some(index) = self.find_slot_for_index(endpoint.index) else {
            return AckRelease::Absent;
        };
        let slot = &self.slots[index];
        if slot.endpoint_generation.load(Ordering::Relaxed) != endpoint.generation {
            self.stale_generation_rejections
                .fetch_add(1, Ordering::Relaxed);
            return AckRelease::StaleGeneration;
        }
        // Terminal states are checked BEFORE the waiter compare, because both of them wipe
        // the waiter identity: comparing first would report a spent lease as a foreign one.
        match slot.state.load(Ordering::Acquire) {
            SLOT_CONSUMED => {
                // The direct edge won. Expected — every successful direct delivery consumes
                // the acknowledgement and then removes the waiter it delivered to.
                self.releases_after_consume.fetch_add(1, Ordering::Relaxed);
                return AckRelease::AlreadyConsumed;
            }
            SLOT_RELEASED => {
                self.duplicate_release_rejections
                    .fetch_add(1, Ordering::Relaxed);
                return AckRelease::AlreadyReleased;
            }
            SLOT_RESERVED => {
                // The publisher still owns this slot and will commit or cancel it; its
                // commit re-verifies the waiter identity under the IPC lock, so a removal
                // that lands here makes that re-verify fail and the publisher cancels.
                self.not_committed_rejections
                    .fetch_add(1, Ordering::Relaxed);
                return AckRelease::NotCommitted;
            }
            SLOT_COMMITTED => {}
            _ => return AckRelease::Absent,
        }
        if slot.waiter() != waiter {
            self.foreign_waiter_rejections
                .fetch_add(1, Ordering::Relaxed);
            return AckRelease::ForeignWaiter;
        }
        // Exactly-once: whichever CPU wins this CAS ends the lease; a loser sees `Released`
        // on its own re-read and reports the duplicate instead of retiring it twice.
        if slot
            .state
            .compare_exchange(
                SLOT_COMMITTED,
                SLOT_RELEASED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            self.duplicate_release_rejections
                .fetch_add(1, Ordering::Relaxed);
            return AckRelease::AlreadyReleased;
        }
        let seq = slot.seq.load(Ordering::Relaxed);
        // The lease is over: drop the departed waiter's identity and its userspace
        // destination pointers. The endpoint key and sequence stay, so the spent slot remains
        // findable for duplicate detection and recyclable in publication order.
        slot.waiter_tid.store(0, Ordering::Relaxed);
        slot.waiter_asid.store(0, Ordering::Relaxed);
        slot.payload_user_ptr.store(0, Ordering::Relaxed);
        slot.payload_user_len.store(0, Ordering::Relaxed);
        slot.meta_user_ptr.store(0, Ordering::Relaxed);
        slot.meta_user_len.store(0, Ordering::Relaxed);
        self.releases.fetch_add(1, Ordering::Relaxed);
        AckRelease::Released(seq)
    }

    /// Re-arm a consumed acknowledgement for a retryable rollback of the SAME publication.
    ///
    /// Matching is by publication sequence, so a superseded publication (the slot was
    /// released and re-used, or a newer commit assigned a fresh sequence) can never be
    /// restored — the stale acknowledgement stays consumed and is discarded.
    pub(crate) fn restore(&self, seq: u64) -> bool {
        if seq == 0 {
            return false;
        }
        for slot in &self.slots {
            if slot.state.load(Ordering::Acquire) == SLOT_CONSUMED
                && slot.seq.load(Ordering::Relaxed) == seq
                && slot
                    .state
                    .compare_exchange(
                        SLOT_CONSUMED,
                        SLOT_COMMITTED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
            {
                return true;
            }
        }
        false
    }

    /// The committed (or already consumed) acknowledgement for this endpoint incarnation,
    /// without transferring ownership. `None` for an absent, stale or still-reserved pair.
    pub(crate) fn snapshot(&self, endpoint: AckEndpoint) -> Option<AckFields> {
        let index = self.find_slot_for_index(endpoint.index)?;
        let slot = &self.slots[index];
        if slot.endpoint_generation.load(Ordering::Relaxed) != endpoint.generation {
            return None;
        }
        match slot.state.load(Ordering::Acquire) {
            SLOT_COMMITTED | SLOT_CONSUMED => Some(slot.fields()),
            _ => None,
        }
    }

    /// True iff an unconsumed committed acknowledgement is present for this endpoint
    /// incarnation.
    pub(crate) fn is_claimable(&self, endpoint: AckEndpoint) -> bool {
        let Some(index) = self.find_slot_for_index(endpoint.index) else {
            return false;
        };
        let slot = &self.slots[index];
        slot.endpoint_generation.load(Ordering::Relaxed) == endpoint.generation
            && slot.state.load(Ordering::Acquire) == SLOT_COMMITTED
    }

    /// The publication sequence of the pair held for this endpoint incarnation, or 0.
    pub(crate) fn commit_seq(&self, endpoint: AckEndpoint) -> u64 {
        let Some(index) = self.find_slot_for_index(endpoint.index) else {
            return 0;
        };
        let slot = &self.slots[index];
        if slot.endpoint_generation.load(Ordering::Relaxed) != endpoint.generation {
            return 0;
        }
        match slot.state.load(Ordering::Acquire) {
            SLOT_COMMITTED | SLOT_CONSUMED => slot.seq.load(Ordering::Relaxed),
            _ => 0,
        }
    }

    /// Forcibly return the slot holding this endpoint index to `Vacant`, whatever its
    /// state, wiping every identity. Returns `true` if a slot was released.
    ///
    /// This is NOT part of the pair lifecycle — a live pair must resolve through
    /// consume/cancel. It exists so a store can be recycled for a fresh pair on the same
    /// endpoint index (hosted fixtures) without resetting counters.
    #[allow(dead_code)]
    pub(crate) fn release_endpoint_index(&self, endpoint_index: usize) -> bool {
        self.lock_admission();
        let released = match self.find_slot_for_index(endpoint_index) {
            Some(index) => {
                self.release_slot(index);
                true
            }
            None => false,
        };
        self.unlock_admission();
        released
    }

    /// Return one slot to `Vacant`, wiping every identity and invalidating any outstanding
    /// reservation token for it. Callers hold the admission guard.
    fn release_slot(&self, index: usize) {
        let slot = &self.slots[index];
        slot.slot_generation.fetch_add(1, Ordering::Relaxed);
        slot.clear_fields();
        slot.state.store(SLOT_VACANT, Ordering::Release);
    }

    // ── Introspection ────────────────────────────────────────────────────────────────
    //
    // Counters and probes over the store's state. Production reads only the two refusal
    // counters (through `overwrite_fuse_count` / `capacity_refusal_count` on the ack
    // modules) and `live_pair_count`; the rest exist so the hosted lifecycle tests and
    // races can assert leak-freedom and rejection classification directly. They are part of
    // the store's contract, not scaffolding, so they stay compiled in every build.

    /// The endpoint incarnation of the store's SINGLE occupied slot, when exactly one slot
    /// is occupied; `None` for zero or more than one.
    ///
    /// Test support only: it lets a wiring fixture that publishes exactly one pair address
    /// that pair through the ordinary endpoint-keyed API without re-deriving the endpoint
    /// identity. It is not an ambient lookup — a store holding two pairs returns `None`.
    #[allow(dead_code)]
    pub(crate) fn sole_pair_endpoint(&self) -> Option<AckEndpoint> {
        let mut found = None;
        for slot in &self.slots {
            if slot.state.load(Ordering::Acquire) == SLOT_VACANT {
                continue;
            }
            if found.is_some() {
                return None;
            }
            found = Some(slot.endpoint());
        }
        found
    }

    /// Number of slots holding a live (reserved or committed) pair.
    pub(crate) fn live_pair_count(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_live()).count()
    }

    /// Number of slots holding a published, not-yet-consumed acknowledgement.
    #[allow(dead_code)]
    pub(crate) fn committed_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.state.load(Ordering::Acquire) == SLOT_COMMITTED)
            .count()
    }

    /// Number of slots holding a reservation that has not published.
    #[allow(dead_code)]
    pub(crate) fn reserved_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.state.load(Ordering::Acquire) == SLOT_RESERVED)
            .count()
    }

    /// Number of slots occupied by anything at all (live or consumed) — the leak probe.
    #[allow(dead_code)]
    pub(crate) fn occupied_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.state.load(Ordering::Acquire) != SLOT_VACANT)
            .count()
    }

    /// True iff no slot retains this waiter incarnation in any state — the waiter-leak
    /// probe for a rolled-back pair.
    #[allow(dead_code)]
    pub(crate) fn waiter_is_absent(&self, waiter: AckWaiter) -> bool {
        self.slots.iter().all(|slot| {
            slot.state.load(Ordering::Acquire) == SLOT_VACANT || slot.waiter() != waiter
        })
    }

    /// Reservations refused because every slot was occupied.
    pub(crate) fn capacity_refusal_count(&self) -> u64 {
        self.capacity_refusals.load(Ordering::Acquire)
    }

    /// Reservations refused because the endpoint already owned a live pair — the
    /// fail-closed successor of the single-slot overwrite fuse. Must be 0 in the sealed
    /// oracle boot.
    pub(crate) fn endpoint_live_refusal_count(&self) -> u64 {
        self.endpoint_live_refusals.load(Ordering::Acquire)
    }

    /// Consumptions rejected because the endpoint generation did not match.
    #[allow(dead_code)]
    pub(crate) fn stale_generation_rejection_count(&self) -> u64 {
        self.stale_generation_rejections.load(Ordering::Acquire)
    }

    /// Consumptions rejected because the waiter incarnation did not match.
    #[allow(dead_code)]
    pub(crate) fn foreign_waiter_rejection_count(&self) -> u64 {
        self.foreign_waiter_rejections.load(Ordering::Acquire)
    }

    /// Consumptions rejected because the pair had not published yet.
    #[allow(dead_code)]
    pub(crate) fn not_committed_rejection_count(&self) -> u64 {
        self.not_committed_rejections.load(Ordering::Acquire)
    }

    /// Consumptions rejected because the pair was already consumed.
    #[allow(dead_code)]
    pub(crate) fn duplicate_consume_rejection_count(&self) -> u64 {
        self.duplicate_consume_rejections.load(Ordering::Acquire)
    }

    /// Total successful reservations (the reserve half of the lifecycle).
    #[allow(dead_code)]
    pub(crate) fn reserve_count(&self) -> u64 {
        self.reserves.load(Ordering::Acquire)
    }

    /// Highest simultaneous live-pair occupancy observed. Bounded by
    /// [`DIRECT_ACK_STORE_CAPACITY`].
    #[allow(dead_code)]
    pub(crate) fn occupancy_high_watermark(&self) -> usize {
        self.occupancy_high_watermark.load(Ordering::Acquire)
    }

    /// Total successful publications.
    #[allow(dead_code)]
    pub(crate) fn commit_count(&self) -> u64 {
        self.commits.load(Ordering::Acquire)
    }

    /// Total successful ownership transfers.
    #[allow(dead_code)]
    pub(crate) fn consume_count(&self) -> u64 {
        self.consumes.load(Ordering::Acquire)
    }

    /// Total leases retired through the non-direct terminal edge.
    pub(crate) fn release_count(&self) -> u64 {
        self.releases.load(Ordering::Acquire)
    }

    /// Releases that found the pair already consumed by the direct edge. Benign — one per
    /// successful direct delivery — and the bridge term in the terminal balance.
    pub(crate) fn release_after_consume_count(&self) -> u64 {
        self.releases_after_consume.load(Ordering::Acquire)
    }

    /// Releases rejected because the lease had already been released.
    pub(crate) fn duplicate_release_rejection_count(&self) -> u64 {
        self.duplicate_release_rejections.load(Ordering::Acquire)
    }

    /// Consumptions refused because the lease had already ended through the non-direct edge.
    /// The mutual-exclusion counter.
    pub(crate) fn crossed_terminal_rejection_count(&self) -> u64 {
        self.crossed_terminal_rejections.load(Ordering::Acquire)
    }

    /// Number of slots holding a lease that has been released (spent, never consumed).
    pub(crate) fn released_pair_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.state.load(Ordering::Acquire) == SLOT_RELEASED)
            .count()
    }

    /// Total cancelled reservations.
    #[allow(dead_code)]
    pub(crate) fn cancel_count(&self) -> u64 {
        self.cancels.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(index: usize, generation: u64) -> AckEndpoint {
        AckEndpoint::new(index, generation)
    }

    fn waiter(tid: u64, asid: u16) -> AckWaiter {
        AckWaiter::new(tid, asid)
    }

    fn fields(e: AckEndpoint, w: AckWaiter) -> AckFields {
        AckFields {
            endpoint: e,
            waiter: w,
            payload_user_ptr: 0x1000 + e.index,
            payload_user_len: 128,
            meta_user_ptr: 0x2000 + e.index,
            meta_user_len: 32,
        }
    }

    fn store() -> DirectAckStore {
        DirectAckStore::new()
    }

    /// Two independent pairs coexist, and each is consumed by its own exact endpoint
    /// identity — the property the single-slot store could not provide.
    #[test]
    fn two_simultaneous_pairs_coexist_and_consume_independently() {
        let s = store();
        let (e1, w1) = (ep(3, 7), waiter(11, 1));
        let (e2, w2) = (ep(4, 9), waiter(12, 2));
        let r1 = s.reserve(e1, w1).expect("first reservation");
        let r2 = s.reserve(e2, w2).expect("second reservation");
        assert_ne!(r1.slot(), r2.slot(), "distinct pairs occupy distinct slots");
        let seq1 = s.commit(r1, fields(e1, w1)).expect("commit first");
        let seq2 = s.commit(r2, fields(e2, w2)).expect("commit second");
        assert_ne!(seq1, seq2);
        assert_eq!(s.committed_count(), 2);

        // Consuming one pair leaves the other untouched and claimable.
        let (got1, got_seq1) = s.consume(e1, Some(w1)).ok().expect("consume first");
        assert_eq!(got1, fields(e1, w1));
        assert_eq!(got_seq1, seq1);
        assert!(s.is_claimable(e2), "second pair survives the first consume");
        let (got2, got_seq2) = s.consume(e2, Some(w2)).ok().expect("consume second");
        assert_eq!(got2, fields(e2, w2));
        assert_eq!(got_seq2, seq2);
        assert_eq!(s.endpoint_live_refusal_count(), 0);
        assert_eq!(s.capacity_refusal_count(), 0);
    }

    /// A pair published for one endpoint cannot be consumed through another endpoint.
    #[test]
    fn consumption_is_keyed_by_exact_endpoint_index() {
        let s = store();
        let (e1, w1) = (ep(3, 7), waiter(11, 1));
        let (e2, w2) = (ep(4, 9), waiter(12, 2));
        s.commit(s.reserve(e1, w1).unwrap(), fields(e1, w1))
            .unwrap();
        s.commit(s.reserve(e2, w2).unwrap(), fields(e2, w2))
            .unwrap();
        // Endpoint 4's consumer never receives endpoint 3's acknowledgement.
        let (got, _) = s.consume(e2, None).ok().expect("consume endpoint 4");
        assert_eq!(got.endpoint, e2);
        assert_eq!(got.waiter, w2);
        assert_eq!(s.consume(ep(5, 1), None), AckConsume::Absent);
    }

    /// A recycled endpoint slot is a different incarnation: the old generation cannot be
    /// consumed by the new one, or vice versa.
    #[test]
    fn stale_endpoint_generation_is_rejected() {
        let s = store();
        let (e, w) = (ep(3, 7), waiter(11, 1));
        s.commit(s.reserve(e, w).unwrap(), fields(e, w)).unwrap();
        assert_eq!(s.consume(ep(3, 8), None), AckConsume::StaleGeneration);
        assert_eq!(s.consume(ep(3, 6), None), AckConsume::StaleGeneration);
        assert_eq!(s.stale_generation_rejection_count(), 2);
        assert!(!s.is_claimable(ep(3, 8)));
        assert_eq!(s.snapshot(ep(3, 8)), None);
        assert_eq!(s.commit_seq(ep(3, 8)), 0);
        // The exact incarnation still works — the rejections mutated nothing.
        assert!(s.consume(e, None).ok().is_some());
    }

    /// A recycled thread id in a different address space is a different waiter incarnation
    /// and cannot consume the earlier incarnation's acknowledgement.
    #[test]
    fn foreign_waiter_incarnation_is_rejected() {
        let s = store();
        let (e, w) = (ep(3, 7), waiter(11, 1));
        s.commit(s.reserve(e, w).unwrap(), fields(e, w)).unwrap();
        assert_eq!(
            s.consume(e, Some(waiter(11, 2))),
            AckConsume::ForeignWaiter,
            "same tid, different asid is a different incarnation"
        );
        assert_eq!(s.consume(e, Some(waiter(12, 1))), AckConsume::ForeignWaiter);
        assert_eq!(s.foreign_waiter_rejection_count(), 2);
        assert!(s.is_claimable(e), "rejections leave the pair claimable");
        assert!(s.consume(e, Some(w)).ok().is_some());
    }

    /// A reservation whose fields name a different incarnation cannot publish.
    #[test]
    fn commit_requires_the_reserved_incarnation() {
        let s = store();
        let (e, w) = (ep(3, 7), waiter(11, 1));
        let r = s.reserve(e, w).unwrap();
        let err = s
            .commit(r, fields(ep(3, 8), w))
            .expect_err("endpoint generation mismatch is refused");
        assert!(matches!(err, AckCommitError::EndpointMismatch(_)));
        let r = err.into_reservation();
        let err = s
            .commit(r, fields(e, waiter(11, 2)))
            .expect_err("waiter incarnation mismatch is refused");
        assert!(matches!(err, AckCommitError::WaiterMismatch(_)));
        let r = err.into_reservation();
        assert_eq!(s.commit_count(), 0, "nothing was published");
        assert!(s.cancel(r));
        assert_eq!(s.occupied_count(), 0);
    }

    /// Exactly-once: the second consumer of the same publication gets nothing.
    #[test]
    fn duplicate_consumption_is_rejected() {
        let s = store();
        let (e, w) = (ep(3, 7), waiter(11, 1));
        s.commit(s.reserve(e, w).unwrap(), fields(e, w)).unwrap();
        assert!(s.consume(e, Some(w)).ok().is_some(), "first consume wins");
        assert_eq!(s.consume(e, Some(w)), AckConsume::AlreadyConsumed);
        assert_eq!(s.consume(e, Some(w)), AckConsume::AlreadyConsumed);
        assert_eq!(s.duplicate_consume_rejection_count(), 2);
        assert_eq!(s.consume_count(), 1, "exactly one ownership transfer");
    }

    /// A reservation is invisible: it cannot be consumed before it publishes.
    #[test]
    fn reserved_but_uncommitted_pair_is_not_consumable() {
        let s = store();
        let (e, w) = (ep(3, 7), waiter(11, 1));
        let r = s.reserve(e, w).unwrap();
        assert_eq!(s.consume(e, Some(w)), AckConsume::NotCommitted);
        assert!(!s.is_claimable(e));
        assert_eq!(s.snapshot(e), None);
        assert_eq!(s.not_committed_rejection_count(), 1);
        assert_eq!(s.reserved_count(), 1);
        s.commit(r, fields(e, w)).unwrap();
        assert!(s.consume(e, Some(w)).ok().is_some());
    }

    /// Capacity is refused at reservation time — before anything is published — and the
    /// existing pairs are untouched.
    #[test]
    fn capacity_exhaustion_refuses_before_publication() {
        let s = store();
        let mut seqs = [0u64; DIRECT_ACK_STORE_CAPACITY];
        for i in 0..DIRECT_ACK_STORE_CAPACITY {
            let (e, w) = (ep(i, i as u64 + 1), waiter(100 + i as u64, 1));
            seqs[i] = s.commit(s.reserve(e, w).unwrap(), fields(e, w)).unwrap();
        }
        assert_eq!(s.live_pair_count(), DIRECT_ACK_STORE_CAPACITY);
        let overflow_ep = ep(DIRECT_ACK_STORE_CAPACITY, 99);
        let overflow_waiter = waiter(999, 3);
        assert_eq!(
            s.reserve(overflow_ep, overflow_waiter),
            Err(AckReserveError::CapacityExhausted)
        );
        assert_eq!(s.capacity_refusal_count(), 1);
        assert_eq!(
            s.commit_count(),
            DIRECT_ACK_STORE_CAPACITY as u64,
            "the refused pair published nothing"
        );
        assert!(
            s.waiter_is_absent(overflow_waiter),
            "a refused reservation leaves no waiter identity behind"
        );
        assert_eq!(s.consume(overflow_ep, None), AckConsume::Absent);
        // Every earlier pair is intact and independently consumable.
        for i in 0..DIRECT_ACK_STORE_CAPACITY {
            let (e, w) = (ep(i, i as u64 + 1), waiter(100 + i as u64, 1));
            let (got, seq) = s.consume(e, Some(w)).ok().expect("earlier pair intact");
            assert_eq!(got, fields(e, w));
            assert_eq!(seq, seqs[i]);
        }
        // Every pair is now SPENT, so the capacity bound (simultaneously LIVE pairs) is
        // clear again and the previously refused pair is admitted by recycling a spent
        // slot. Capacity is refused, not poisoned.
        assert_eq!(s.live_pair_count(), 0);
        let r = s
            .reserve(overflow_ep, overflow_waiter)
            .expect("a spent slot is recycled");
        s.commit(r, fields(overflow_ep, overflow_waiter)).unwrap();
        assert_eq!(s.capacity_refusal_count(), 1, "no further refusal");
        assert!(s.consume(overflow_ep, Some(overflow_waiter)).ok().is_some());
    }

    /// One endpoint index never occupies two slots: a spent pair on that endpoint is
    /// recycled in place rather than duplicated, so the fresh pair is the one consumed.
    #[test]
    fn a_spent_pair_is_recycled_in_place_for_the_same_endpoint() {
        let s = store();
        let (e1, w1) = (ep(3, 7), waiter(11, 1));
        let seq1 = s
            .commit(s.reserve(e1, w1).unwrap(), fields(e1, w1))
            .unwrap();
        s.consume(e1, Some(w1))
            .ok()
            .expect("consume the first pair");
        assert_eq!(s.occupied_count(), 1);
        // A NEW incarnation of the same endpoint index.
        let (e2, w2) = (ep(3, 8), waiter(12, 2));
        let seq2 = s
            .commit(s.reserve(e2, w2).unwrap(), fields(e2, w2))
            .unwrap();
        assert_ne!(seq1, seq2);
        assert_eq!(s.occupied_count(), 1, "one endpoint index, one slot");
        // The superseded publication is gone; only the fresh incarnation is consumable.
        assert!(
            !s.restore(seq1),
            "a superseded publication cannot be restored"
        );
        assert_eq!(s.consume(e1, Some(w1)), AckConsume::StaleGeneration);
        let (got, got_seq) = s.consume(e2, Some(w2)).ok().expect("fresh pair");
        assert_eq!(got.waiter, w2);
        assert_eq!(got_seq, seq2);
    }

    /// A second live pair for the SAME endpoint is refused; the published acknowledgement
    /// survives untouched. This is the successor of the single-slot overwrite fuse.
    #[test]
    fn second_live_pair_on_one_endpoint_is_refused() {
        let s = store();
        let (e, w) = (ep(3, 7), waiter(11, 1));
        let seq = s.commit(s.reserve(e, w).unwrap(), fields(e, w)).unwrap();
        assert_eq!(
            s.reserve(e, waiter(12, 2)),
            Err(AckReserveError::EndpointAlreadyLive)
        );
        assert_eq!(s.endpoint_live_refusal_count(), 1);
        let (got, got_seq) = s.consume(e, Some(w)).ok().expect("original ack preserved");
        assert_eq!(got.waiter, w);
        assert_eq!(got_seq, seq);
        // Once consumed, the endpoint accepts a fresh pair.
        assert!(s.reserve(e, waiter(12, 2)).is_ok());
    }

    /// Cancelling a reservation leaves no slot occupied and no waiter identity readable.
    #[test]
    fn cancel_leaves_no_slot_or_waiter_leak() {
        let s = store();
        let (e, w) = (ep(3, 7), waiter(11, 1));
        let r = s.reserve(e, w).unwrap();
        assert_eq!(s.occupied_count(), 1);
        assert!(s.cancel(r));
        assert_eq!(s.occupied_count(), 0, "no slot leak");
        assert_eq!(s.live_pair_count(), 0);
        assert!(s.waiter_is_absent(w), "no waiter leak");
        assert_eq!(s.consume(e, None), AckConsume::Absent);
        assert_eq!(s.cancel_count(), 1);
        // Full capacity is available again after the rollback.
        for i in 0..DIRECT_ACK_STORE_CAPACITY {
            assert!(s.reserve(ep(i, 1), waiter(i as u64, 1)).is_ok());
        }
    }

    /// A stale reservation token (its slot was released underneath it) can neither commit
    /// nor cancel someone else's pair.
    #[test]
    fn stale_reservation_token_cannot_commit_or_cancel() {
        let s = store();
        let (e, w) = (ep(3, 7), waiter(11, 1));
        let stale = s.reserve(e, w).unwrap();
        assert!(s.release_endpoint_index(3));
        // A new pair takes the same slot.
        let (e2, w2) = (ep(3, 8), waiter(12, 2));
        let fresh = s.reserve(e2, w2).unwrap();
        assert_eq!(stale.slot(), fresh.slot(), "same physical slot");
        let err = s
            .commit(stale, fields(e, w))
            .expect_err("stale token cannot publish");
        assert!(matches!(err, AckCommitError::Stale(_)));
        assert!(
            !s.cancel(err.into_reservation()),
            "stale token cannot cancel the fresh pair"
        );
        // The fresh pair is unharmed.
        s.commit(fresh, fields(e2, w2)).unwrap();
        assert!(s.consume(e2, Some(w2)).ok().is_some());
    }

    /// Restore re-arms the SAME publication only.
    #[test]
    fn restore_rearms_only_the_same_publication() {
        let s = store();
        let (e, w) = (ep(3, 7), waiter(11, 1));
        let seq = s.commit(s.reserve(e, w).unwrap(), fields(e, w)).unwrap();
        let (_, claimed) = s.consume(e, Some(w)).ok().expect("consume");
        assert_eq!(claimed, seq);
        assert!(!s.is_claimable(e));
        assert!(s.restore(seq), "same publication is restorable");
        assert!(s.is_claimable(e));
        assert!(
            !s.restore(seq.wrapping_add(1)),
            "unknown seq is not restorable"
        );
        assert!(!s.restore(0), "the null sequence is never restorable");
        // Restoring an already-committed (unconsumed) pair is a no-op refusal.
        assert!(!s.restore(seq));
        assert!(s.consume(e, Some(w)).ok().is_some(), "re-consumable once");
    }

    /// Restore across multiple pairs re-arms exactly the matching one.
    #[test]
    fn restore_selects_the_matching_pair_among_many() {
        let s = store();
        let (e1, w1) = (ep(1, 1), waiter(11, 1));
        let (e2, w2) = (ep(2, 2), waiter(12, 1));
        let seq1 = s
            .commit(s.reserve(e1, w1).unwrap(), fields(e1, w1))
            .unwrap();
        let seq2 = s
            .commit(s.reserve(e2, w2).unwrap(), fields(e2, w2))
            .unwrap();
        s.consume(e1, Some(w1)).ok().expect("consume first");
        s.consume(e2, Some(w2)).ok().expect("consume second");
        assert!(s.restore(seq2));
        assert!(!s.is_claimable(e1), "the other pair stays consumed");
        assert!(s.is_claimable(e2));
        assert_eq!(s.commit_seq(e2), seq2);
        assert_eq!(s.commit_seq(e1), seq1, "a consumed pair keeps its sequence");
    }

    /// Reset invalidates outstanding reservations and clears every identity.
    #[test]
    fn reset_clears_state_and_invalidates_tokens() {
        let s = store();
        let (e, w) = (ep(3, 7), waiter(11, 1));
        let r = s.reserve(e, w).unwrap();
        s.reset();
        assert_eq!(s.occupied_count(), 0);
        assert!(s.waiter_is_absent(w));
        let err = s
            .commit(r, fields(e, w))
            .expect_err("token did not survive");
        assert!(matches!(err, AckCommitError::Stale(_)));
        assert!(!s.cancel(err.into_reservation()));
        assert_eq!(s.commit_count(), 0);
    }
}
