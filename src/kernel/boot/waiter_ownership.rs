// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199D-WA2A-R1 — the generation-bearing endpoint-waiter ownership primitive.
//!
//! # Why
//!
//! `WAITER_OWNERSHIP_EXCLUSIVE=no`. The endpoint receive-waiter table is the *de facto* arbiter
//! between the paths that can wake a `Blocked(EndpointReceive)` task — but only for the paths
//! that happen to consult it, and the independent status-transition census in
//! `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.32 shows that most of them do not. That is why the x86
//! direct production default is OFF.
//!
//! This module is the single typed primitive a later increment can route every one of those
//! paths through. **It is helper-only: zero production call sites in this increment.**
//!
//! # Structural bound, not an associative cache
//!
//! The table is **endpoint-indexed**: one slot per endpoint index, with
//! `WAITER_OWNERSHIP_SLOTS` derived from [`ENDPOINT_WAITER_SLOTS`] and pinned by a compile-time
//! assertion so the two can never drift. That is the same structural argument
//! [`crate::kernel::direct_ack_store`] makes about acknowledgement leases: at most one endpoint
//! receive-waiter can exist per endpoint index, so at most one ownership claim can either.
//!
//! An associative table keyed on the *incarnation* would be bounded in size but unbounded in
//! lifetime — N sequential completed waits, each with a fresh wait generation, would exhaust an
//! N-slot table with zero live claims. Here a finished incarnation occupies its slot only until
//! the **next** incarnation of that endpoint index claims it.
//!
//! # Lock discipline is structural, not documentary
//!
//! [`WaiterOwnershipTable`] is private state of [`IpcSubsystem`], so reaching it requires the ipc
//! rank-3 guard the caller already holds. Its own claim/settle methods are **module-private**:
//! the only cross-module surface is the typed `IpcSubsystem::waiter_ownership_*` methods below,
//! so a standalone table is useless even where one is nameable. The module acquires nothing and
//! calls into no other domain, so it cannot nest task(2) or scheduler(1) beneath ipc(3).
//! [`WaiterClaimToken`] is opaque and `Copy`, so it survives the guard being released.

// HELPER-ONLY marker, matching the split-seam convention elsewhere in the tree: this primitive
// has ZERO production call sites in this increment, so every item is dead outside tests. The
// attribute is the honest statement of that, and `the_primitive_has_no_production_caller` is what
// enforces it — remove the attribute only in the increment that wires the first real caller.
#![cfg_attr(not(test), allow(dead_code))]

use super::ENDPOINT_WAITER_SLOTS;
use super::defs::{IpcSubsystem, ReceiverWaiterIdentity};

/// One ownership slot per endpoint index. **Derived**, never a second numeric literal — the
/// assertion below makes an accidental redefinition a compile error rather than a silent
/// capacity split.
pub(crate) const WAITER_OWNERSHIP_SLOTS: usize = ENDPOINT_WAITER_SLOTS;

const _: () = assert!(WAITER_OWNERSHIP_SLOTS == ENDPOINT_WAITER_SLOTS);

/// Which subsystem owns a claim. Naming an owner here does **not** wire that path — every one
/// of these is helper-only in this increment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaiterOwner {
    /// The off-lock NR6 direct request transaction.
    DirectRequest,
    /// The off-lock NR7 direct reply transaction.
    DirectReply,
    /// The ordinary (non-token-bearing) IPC deadline scan.
    OrdinaryTimeout,
    /// Legacy in-lock endpoint delivery: send-to-blocked-receiver, `ipc_reply`, shared region.
    LegacyDelivery,
    /// Notification signal delivery and destroyed-notification wake.
    Notification,
    /// Task teardown: exit, mark-dead, reap, restart.
    Teardown,
}

/// The exact incarnation a claim is about. All four dimensions are load-bearing:
///
/// * `endpoint_index` **and** `endpoint_generation` — a destroyed-and-recreated endpoint reusing
///   the slot is a different key, so a stale token can never restore into it;
/// * `waiter` (`{tid, asid}`) — a replacement task reusing the numeric TID is a different key;
/// * `wait_generation` — the task's blocked-receive generation, so a task that unblocked, ran and
///   reblocked on the same endpoint is a different key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WaiterKey {
    pub(crate) endpoint_index: usize,
    pub(crate) endpoint_generation: u64,
    pub(crate) waiter: ReceiverWaiterIdentity,
    pub(crate) wait_generation: u64,
}

/// What a caller may learn about a slot. Deliberately **does not** carry the live claim
/// generation: knowing it would let a caller reconstruct authority it never earned. Only a
/// successful [`IpcSubsystem::waiter_ownership_claim`] mints a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaiterOwnershipView {
    /// No claim for this incarnation. It is claimable.
    Vacant,
    /// Owned. Only the exact token issued to `owner` may settle it.
    Claimed { owner: WaiterOwner },
    /// Terminal: this exact incarnation was settled as a completed delivery/wake.
    Consumed,
    /// Terminal: this exact incarnation was abandoned without delivery.
    Cancelled,
}

/// An opaque, owned proof of ownership.
///
/// Its fields are **private to this module**, so no struct literal elsewhere in the crate can
/// forge one — only a successful claim mints a token. It is `Copy` and self-contained, so it
/// survives the rank-3 guard being released, which is the whole point: the owner does its user
/// copies and capability work off-lock and settles later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WaiterClaimToken {
    key: WaiterKey,
    owner: WaiterOwner,
    claim_generation: u64,
}

impl WaiterClaimToken {
    /// Which owner earned this claim — so a future owner can log and assert its own identity.
    pub(crate) fn owner(&self) -> WaiterOwner {
        self.owner
    }

    /// The endpoint slot this claim is about — so a future owner can address the waiter table it
    /// is arbitrating over.
    pub(crate) fn endpoint_index(&self) -> usize {
        self.key.endpoint_index
    }

    /// The exact `{tid, asid}` whose blocked receive is owned — so a future owner can address the
    /// TCB it will complete.
    pub(crate) fn waiter(&self) -> ReceiverWaiterIdentity {
        self.key.waiter
    }

    #[cfg(test)]
    fn key(&self) -> WaiterKey {
        self.key
    }

    #[cfg(test)]
    fn claim_generation(&self) -> u64 {
        self.claim_generation
    }

    /// Test-only forgery, so the guards can prove a foreign-owner token is refused. No non-test
    /// path can produce a token with an owner the table did not issue it to.
    #[cfg(test)]
    fn forged_with_owner(&self, owner: WaiterOwner) -> Self {
        Self { owner, ..*self }
    }

    /// Test-only forgery for a different incarnation.
    #[cfg(test)]
    fn forged_with_key(&self, key: WaiterKey) -> Self {
        Self { key, ..*self }
    }
}

/// Why a claim was refused. Every variant is fail-closed: nothing is evicted, overwritten or
/// stamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimError {
    /// The key names an endpoint index the table does not have. Fail closed rather than wrap or
    /// panic — an out-of-range index is a caller bug, not a capacity event.
    EndpointIndexOutOfRange { index: usize },
    /// The slot holds a live claim — for this incarnation or another one. A live claim is
    /// **never** evicted, so this is returned for both.
    AlreadyClaimed { by: WaiterOwner },
    /// Terminal — this exact incarnation was already delivered.
    Consumed,
    /// Terminal — this exact incarnation was already abandoned.
    Cancelled,
    /// The strictly increasing claim generation cannot advance without wrapping. Fail closed:
    /// the slot and the counter are untouched and no token is minted, because a wrapped
    /// generation would make an ancient token valid again.
    ClaimGenerationExhausted,
}

/// Why a settle (consume / restore / cancel) was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettleError {
    /// The token names an endpoint index the table does not have.
    EndpointIndexOutOfRange { index: usize },
    /// The slot does not hold this exact key — vacant, or already taken over by a later
    /// incarnation. A recycled endpoint generation, a replacement task and a re-blocked task all
    /// land here, which is precisely what stops a stale restore.
    NoSuchClaim,
    /// The slot is claimed for this exact incarnation, but by a different owner.
    ForeignOwner { by: WaiterOwner },
    /// The right owner and the right incarnation, but an older claim than the live one: a stale
    /// token. The live generation is deliberately **not** reported.
    StaleClaimGeneration,
    /// The slot holds this exact key in a terminal state.
    NotClaimed { state: WaiterOwnershipView },
}

/// One endpoint index's ownership state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaiterOwnershipSlot {
    /// Nothing is recorded for this endpoint index.
    Vacant,
    /// A live claim. Only the exact `{key, owner, claim_generation}` may settle it.
    Claimed {
        key: WaiterKey,
        owner: WaiterOwner,
        claim_generation: u64,
    },
    /// The named incarnation completed. Terminal **for that incarnation only** — a later
    /// incarnation of the same endpoint index takes the slot over.
    Consumed { key: WaiterKey },
    /// The named incarnation was abandoned. Terminal for that incarnation only.
    Cancelled { key: WaiterKey },
}

impl WaiterOwnershipSlot {
    /// The key this slot is about, if it is about one.
    fn key(&self) -> Option<&WaiterKey> {
        match self {
            Self::Vacant => None,
            Self::Claimed { key, .. } | Self::Consumed { key } | Self::Cancelled { key } => {
                Some(key)
            }
        }
    }
}

/// The endpoint-indexed ownership table. Private state of [`IpcSubsystem`]: it holds no lock of
/// its own, and every entry point is reached through the caller's ipc rank-3 guard.
#[derive(Debug)]
pub(crate) struct WaiterOwnershipTable {
    slots: [WaiterOwnershipSlot; WAITER_OWNERSHIP_SLOTS],
    /// Strictly increasing, starting at 1 and advanced by `checked_add`. Stamped into every
    /// claim so a token from an earlier claim of the same incarnation is recognisably stale.
    /// Zero is never issued, and a wrapped value can never be issued.
    next_claim_generation: u64,
}

impl WaiterOwnershipTable {
    /// The empty table. `const`, so the single [`IpcSubsystem`] initializer builds it in place
    /// exactly like `TerminalCell::vacant()` / `DeadlineTokenCell::vacant()` — no `unsafe`, no
    /// zero initialization. Visibility stops at the boot domain: this is the only item in the
    /// module that is nameable outside it *and* produces a table.
    pub(in crate::kernel::boot) const fn vacant() -> Self {
        Self {
            slots: [WaiterOwnershipSlot::Vacant; WAITER_OWNERSHIP_SLOTS],
            next_claim_generation: 1,
        }
    }

    /// Advance the claim generation, or fail closed. Called only after every other check has
    /// passed, so a failure here leaves the slot exactly as it was.
    fn stamp(&mut self) -> Result<u64, ClaimError> {
        let generation = self.next_claim_generation;
        let next = generation
            .checked_add(1)
            .ok_or(ClaimError::ClaimGenerationExhausted)?;
        debug_assert!(generation != 0, "generation 0 is never a valid claim");
        self.next_claim_generation = next;
        Ok(generation)
    }

    fn claim(
        &mut self,
        key: WaiterKey,
        owner: WaiterOwner,
    ) -> Result<WaiterClaimToken, ClaimError> {
        let index = key.endpoint_index;
        let slot = self
            .slots
            .get(index)
            .ok_or(ClaimError::EndpointIndexOutOfRange { index })?;
        match slot {
            // A live claim is never evicted — not for a foreign incarnation, and not for this
            // one. The caller learns who holds it, so a duplicate by the SAME owner is
            // distinguishable from a genuine collision.
            WaiterOwnershipSlot::Claimed { owner: by, .. } => {
                return Err(ClaimError::AlreadyClaimed { by: *by });
            }
            // Terminal, but only for the incarnation it names: the exact incarnation keeps its
            // result, while any other incarnation of this endpoint index may take the slot over.
            WaiterOwnershipSlot::Consumed { key: held } if *held == key => {
                return Err(ClaimError::Consumed);
            }
            WaiterOwnershipSlot::Cancelled { key: held } if *held == key => {
                return Err(ClaimError::Cancelled);
            }
            WaiterOwnershipSlot::Vacant
            | WaiterOwnershipSlot::Consumed { .. }
            | WaiterOwnershipSlot::Cancelled { .. } => {}
        }
        let claim_generation = self.stamp()?;
        self.slots[index] = WaiterOwnershipSlot::Claimed {
            key,
            owner,
            claim_generation,
        };
        Ok(WaiterClaimToken {
            key,
            owner,
            claim_generation,
        })
    }

    /// Validate a token against the live slot. Every dimension is compared: the key (all four of
    /// its fields, by `PartialEq`), the owner, and the claim generation.
    fn validate(&self, token: &WaiterClaimToken) -> Result<usize, SettleError> {
        let index = token.key.endpoint_index;
        let slot = self
            .slots
            .get(index)
            .ok_or(SettleError::EndpointIndexOutOfRange { index })?;
        match slot {
            WaiterOwnershipSlot::Claimed {
                key,
                owner,
                claim_generation,
            } => {
                if *key != token.key {
                    // The slot moved on to a later incarnation: this token was evicted.
                    return Err(SettleError::NoSuchClaim);
                }
                if *owner != token.owner {
                    return Err(SettleError::ForeignOwner { by: *owner });
                }
                if *claim_generation != token.claim_generation {
                    return Err(SettleError::StaleClaimGeneration);
                }
                Ok(index)
            }
            WaiterOwnershipSlot::Consumed { key } if *key == token.key => {
                Err(SettleError::NotClaimed {
                    state: WaiterOwnershipView::Consumed,
                })
            }
            WaiterOwnershipSlot::Cancelled { key } if *key == token.key => {
                Err(SettleError::NotClaimed {
                    state: WaiterOwnershipView::Cancelled,
                })
            }
            _ => Err(SettleError::NoSuchClaim),
        }
    }

    /// Terminal success: the owner delivered, so this exact incarnation may never be re-armed.
    fn consume(&mut self, token: WaiterClaimToken) -> Result<(), SettleError> {
        let index = self.validate(&token)?;
        self.slots[index] = WaiterOwnershipSlot::Consumed { key: token.key };
        Ok(())
    }

    /// Non-terminal rollback: the owner did not deliver. The slot returns to `Vacant` rather than
    /// retaining the old key, so nothing is held against a future incarnation — and the token
    /// just spent is already invalid, because a re-claim mints a fresh generation.
    fn restore(&mut self, token: WaiterClaimToken) -> Result<(), SettleError> {
        let index = self.validate(&token)?;
        self.slots[index] = WaiterOwnershipSlot::Vacant;
        Ok(())
    }

    /// Terminal abandonment: the incarnation is dead (task exited, endpoint destroyed).
    fn cancel(&mut self, token: WaiterClaimToken) -> Result<(), SettleError> {
        let index = self.validate(&token)?;
        self.slots[index] = WaiterOwnershipSlot::Cancelled { key: token.key };
        Ok(())
    }

    fn view(&self, key: &WaiterKey) -> WaiterOwnershipView {
        let Some(slot) = self.slots.get(key.endpoint_index) else {
            return WaiterOwnershipView::Vacant;
        };
        if slot.key() != Some(key) {
            // The slot is about a different incarnation, so THIS one is unclaimed.
            return WaiterOwnershipView::Vacant;
        }
        match slot {
            WaiterOwnershipSlot::Vacant => WaiterOwnershipView::Vacant,
            WaiterOwnershipSlot::Claimed { owner, .. } => {
                WaiterOwnershipView::Claimed { owner: *owner }
            }
            WaiterOwnershipSlot::Consumed { .. } => WaiterOwnershipView::Consumed,
            WaiterOwnershipSlot::Cancelled { .. } => WaiterOwnershipView::Cancelled,
        }
    }

    /// Live (claimed, non-terminal) entries. Diagnostic only.
    fn claimed_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| matches!(s, WaiterOwnershipSlot::Claimed { .. }))
            .count()
    }
}

/// The **entire** cross-module surface of the primitive. Every one of these requires
/// `&mut IpcSubsystem` (or `&IpcSubsystem`), which only the ipc rank-3 guard hands out, and the
/// table's own methods are private to this module — so ownership cannot be mutated through the
/// task, scheduler, capability, VM or broad-state APIs at all.
impl IpcSubsystem {
    pub(crate) fn waiter_ownership_claim(
        &mut self,
        key: WaiterKey,
        owner: WaiterOwner,
    ) -> Result<WaiterClaimToken, ClaimError> {
        self.waiter_ownership.claim(key, owner)
    }

    pub(crate) fn waiter_ownership_consume(
        &mut self,
        token: WaiterClaimToken,
    ) -> Result<(), SettleError> {
        self.waiter_ownership.consume(token)
    }

    pub(crate) fn waiter_ownership_restore(
        &mut self,
        token: WaiterClaimToken,
    ) -> Result<(), SettleError> {
        self.waiter_ownership.restore(token)
    }

    pub(crate) fn waiter_ownership_cancel(
        &mut self,
        token: WaiterClaimToken,
    ) -> Result<(), SettleError> {
        self.waiter_ownership.cancel(token)
    }

    pub(crate) fn waiter_ownership_view(&self, key: &WaiterKey) -> WaiterOwnershipView {
        self.waiter_ownership.view(key)
    }

    pub(crate) fn waiter_ownership_claimed_count(&self) -> usize {
        self.waiter_ownership.claimed_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::vm::Asid;

    fn key(eidx: usize, egen: u64, tid: u64, asid: u16, wait_gen: u64) -> WaiterKey {
        WaiterKey {
            endpoint_index: eidx,
            endpoint_generation: egen,
            waiter: ReceiverWaiterIdentity::new(ThreadId(tid), Asid(asid)),
            wait_generation: wait_gen,
        }
    }

    /// The canonical key every test varies one dimension of.
    fn base() -> WaiterKey {
        key(3, 7, 42, 2, 11)
    }

    fn table() -> WaiterOwnershipTable {
        WaiterOwnershipTable::vacant()
    }

    // ── A. Structural bound, no lifetime leak ───────────────────────────────────────────────

    /// The capacity is the endpoint waiter table's, derived rather than duplicated.
    #[test]
    fn the_capacity_is_exactly_the_endpoint_waiter_bound() {
        assert_eq!(WAITER_OWNERSHIP_SLOTS, ENDPOINT_WAITER_SLOTS);
        assert_eq!(table().slots.len(), ENDPOINT_WAITER_SLOTS);
    }

    #[test]
    fn every_endpoint_slot_can_hold_a_live_claim_simultaneously() {
        let mut t = table();
        for i in 0..ENDPOINT_WAITER_SLOTS {
            t.claim(key(i, 1, i as u64, 1, 1), WaiterOwner::DirectRequest)
                .unwrap_or_else(|e| panic!("endpoint {i} must claim: {e:?}"));
        }
        assert_eq!(
            t.claimed_count(),
            ENDPOINT_WAITER_SLOTS,
            "the structural bound is reachable, not merely nominal"
        );
    }

    #[test]
    fn an_out_of_range_endpoint_index_fails_closed() {
        let mut t = table();
        let bad = key(ENDPOINT_WAITER_SLOTS, 1, 1, 1, 1);
        assert_eq!(
            t.claim(bad, WaiterOwner::DirectRequest),
            Err(ClaimError::EndpointIndexOutOfRange {
                index: ENDPOINT_WAITER_SLOTS
            })
        );
        assert_eq!(t.claimed_count(), 0, "and nothing was recorded");
        assert_eq!(
            t.view(&bad),
            WaiterOwnershipView::Vacant,
            "an out-of-range key is never claimed"
        );
        // A token whose key is out of range cannot settle either.
        let live = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        assert_eq!(
            t.consume(live.forged_with_key(bad)),
            Err(SettleError::EndpointIndexOutOfRange {
                index: ENDPOINT_WAITER_SLOTS
            })
        );
    }

    /// **The defect this repair exists for.** The old associative table retained a key for every
    /// incarnation it had ever seen, so N sequential *completed* waits exhausted an N-slot table
    /// with zero live claims. Endpoint-indexed, a finished incarnation occupies its slot only
    /// until the next one claims it.
    #[test]
    fn ten_thousand_sequential_claim_restore_cycles_never_exhaust() {
        let mut t = table();
        for wait_generation in 0..10_001u64 {
            let k = WaiterKey {
                wait_generation,
                ..base()
            };
            let token = t
                .claim(k, WaiterOwner::DirectRequest)
                .unwrap_or_else(|e| panic!("wait generation {wait_generation} must claim: {e:?}"));
            assert_eq!(t.restore(token), Ok(()));
            assert_eq!(t.claimed_count(), 0, "claimed_count returns to zero");
        }
        assert_eq!(t.claimed_count(), 0);
    }

    #[test]
    fn ten_thousand_claim_consume_new_incarnation_cycles_never_exhaust() {
        let mut t = table();
        for wait_generation in 0..10_001u64 {
            let k = WaiterKey {
                wait_generation,
                ..base()
            };
            let token = t
                .claim(k, WaiterOwner::DirectReply)
                .unwrap_or_else(|e| panic!("wait generation {wait_generation} must claim: {e:?}"));
            assert_eq!(t.consume(token), Ok(()));
            assert_eq!(t.view(&k), WaiterOwnershipView::Consumed);
        }
        assert_eq!(t.claimed_count(), 0);
    }

    #[test]
    fn ten_thousand_claim_cancel_new_incarnation_cycles_never_exhaust() {
        let mut t = table();
        for wait_generation in 0..10_001u64 {
            let k = WaiterKey {
                wait_generation,
                ..base()
            };
            let token = t
                .claim(k, WaiterOwner::Teardown)
                .unwrap_or_else(|e| panic!("wait generation {wait_generation} must claim: {e:?}"));
            assert_eq!(t.cancel(token), Ok(()));
            assert_eq!(t.view(&k), WaiterOwnershipView::Cancelled);
        }
        assert_eq!(t.claimed_count(), 0);
    }

    #[test]
    fn an_exact_consumed_or_cancelled_incarnation_stays_terminal() {
        for (settle_is_consume, expect_claim, expect_view) in [
            (true, ClaimError::Consumed, WaiterOwnershipView::Consumed),
            (false, ClaimError::Cancelled, WaiterOwnershipView::Cancelled),
        ] {
            let mut t = table();
            let token = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
            if settle_is_consume {
                assert_eq!(t.consume(token), Ok(()));
            } else {
                assert_eq!(t.cancel(token), Ok(()));
            }
            assert_eq!(t.view(&base()), expect_view);
            // Every owner is refused, not just the one that settled it.
            for owner in [
                WaiterOwner::DirectRequest,
                WaiterOwner::OrdinaryTimeout,
                WaiterOwner::Notification,
                WaiterOwner::Teardown,
            ] {
                assert_eq!(t.claim(base(), owner), Err(expect_claim));
            }
            // And the spent token cannot re-arm it.
            assert_eq!(
                t.restore(token),
                Err(SettleError::NotClaimed { state: expect_view })
            );
            assert_eq!(
                t.view(&base()),
                expect_view,
                "a refused settle mutates nothing"
            );
        }
    }

    #[test]
    fn a_new_incarnation_replaces_a_terminal_one_in_every_dimension() {
        for replacement in [
            // a destroyed-and-recreated endpoint at the same index
            WaiterKey {
                endpoint_generation: base().endpoint_generation + 1,
                ..base()
            },
            // the same numeric TID under a new address space
            key(3, 7, 42, 2 ^ 0x55, 11),
            // the same task, unblocked and reblocked
            WaiterKey {
                wait_generation: base().wait_generation + 1,
                ..base()
            },
        ] {
            for terminal_is_consume in [true, false] {
                let mut t = table();
                let token = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
                if terminal_is_consume {
                    assert_eq!(t.consume(token), Ok(()));
                } else {
                    assert_eq!(t.cancel(token), Ok(()));
                }
                assert!(
                    t.claim(replacement, WaiterOwner::LegacyDelivery).is_ok(),
                    "a different incarnation must be able to take the slot over: {replacement:?}"
                );
                assert_eq!(
                    t.view(&base()),
                    WaiterOwnershipView::Vacant,
                    "and the old incarnation's terminal record goes with it"
                );
            }
        }
    }

    #[test]
    fn a_live_claim_is_never_evicted_by_any_other_incarnation() {
        let mut t = table();
        let live = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        for other in [
            WaiterKey {
                endpoint_generation: 999,
                ..base()
            },
            key(3, 7, 42, 0x5a, 11),
            WaiterKey {
                wait_generation: 999,
                ..base()
            },
            base(),
        ] {
            assert_eq!(
                t.claim(other, WaiterOwner::Teardown),
                Err(ClaimError::AlreadyClaimed {
                    by: WaiterOwner::DirectRequest
                }),
                "a live claim outranks every competitor, including a later incarnation: {other:?}"
            );
        }
        assert_eq!(
            t.view(&base()),
            WaiterOwnershipView::Claimed {
                owner: WaiterOwner::DirectRequest
            }
        );
        assert_eq!(
            t.consume(live),
            Ok(()),
            "and the real owner can still settle"
        );
    }

    #[test]
    fn claimed_count_is_zero_after_every_fully_restored_sequence() {
        let mut t = table();
        let mut tokens = alloc::vec::Vec::new();
        for i in 0..ENDPOINT_WAITER_SLOTS {
            tokens.push(
                t.claim(key(i, 1, i as u64, 1, 1), WaiterOwner::DirectRequest)
                    .expect("claim"),
            );
        }
        assert_eq!(t.claimed_count(), ENDPOINT_WAITER_SLOTS);
        for token in tokens {
            assert_eq!(t.restore(token), Ok(()));
        }
        assert_eq!(t.claimed_count(), 0);
        for i in 0..ENDPOINT_WAITER_SLOTS {
            assert_eq!(
                t.view(&key(i, 1, i as u64, 1, 1)),
                WaiterOwnershipView::Vacant
            );
        }
    }

    // ── One winner ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn exactly_one_of_two_competing_owners_wins() {
        let mut t = table();
        let token = t
            .claim(base(), WaiterOwner::DirectRequest)
            .expect("first claim wins");
        assert_eq!(
            t.claim(base(), WaiterOwner::OrdinaryTimeout),
            Err(ClaimError::AlreadyClaimed {
                by: WaiterOwner::DirectRequest
            }),
            "the loser learns WHO holds it, so a foreign collision is distinguishable"
        );
        assert_eq!(t.claimed_count(), 1, "exactly one live claim");
        assert_eq!(t.consume(token), Ok(()), "and only the winner can settle");
    }

    #[test]
    fn a_duplicate_claim_by_the_same_owner_is_refused_and_names_itself() {
        let mut t = table();
        let first = t.claim(base(), WaiterOwner::DirectReply).expect("first");
        assert_eq!(
            t.claim(base(), WaiterOwner::DirectReply),
            Err(ClaimError::AlreadyClaimed {
                by: WaiterOwner::DirectReply
            }),
            "a duplicate is refused — it must not silently re-stamp a fresh generation"
        );
        assert_eq!(t.restore(first), Ok(()), "the original token is still live");
    }

    // ── Foreign owner ───────────────────────────────────────────────────────────────────────

    #[test]
    fn a_foreign_owner_can_neither_consume_nor_restore_nor_cancel() {
        let mut t = table();
        let real = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        let forged = real.forged_with_owner(WaiterOwner::Notification);
        for (what, r) in [
            ("consume", t.consume(forged)),
            ("restore", t.restore(forged)),
            ("cancel", t.cancel(forged)),
        ] {
            assert_eq!(
                r,
                Err(SettleError::ForeignOwner {
                    by: WaiterOwner::DirectRequest
                }),
                "{what} by a foreign owner must be refused"
            );
        }
        assert_eq!(
            t.view(&base()),
            WaiterOwnershipView::Claimed {
                owner: WaiterOwner::DirectRequest
            },
            "a rejected settle mutates nothing"
        );
        assert_eq!(t.consume(real), Ok(()));
    }

    // ── The three identity/generation dimensions ────────────────────────────────────────────

    #[test]
    fn a_stale_token_for_an_evicted_incarnation_is_no_such_claim() {
        for replacement in [
            WaiterKey {
                endpoint_generation: base().endpoint_generation + 1,
                ..base()
            },
            key(3, 7, 42, 2 ^ 0x55, 11),
            WaiterKey {
                wait_generation: base().wait_generation + 1,
                ..base()
            },
        ] {
            let mut t = table();
            let old = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
            assert_eq!(t.restore(old), Ok(()));
            let new = t
                .claim(replacement, WaiterOwner::DirectRequest)
                .expect("the new incarnation claims the same endpoint slot");
            for (what, r) in [
                ("consume", t.consume(old)),
                ("restore", t.restore(old)),
                ("cancel", t.cancel(old)),
            ] {
                assert_eq!(
                    r,
                    Err(SettleError::NoSuchClaim),
                    "{what} with a token for an evicted incarnation must not touch {replacement:?}"
                );
            }
            assert_eq!(
                t.view(&replacement),
                WaiterOwnershipView::Claimed {
                    owner: WaiterOwner::DirectRequest
                },
                "the new incarnation is untouched"
            );
            assert_eq!(t.consume(new), Ok(()));
        }
    }

    // ── Settle semantics ────────────────────────────────────────────────────────────────────

    #[test]
    fn restore_returns_the_slot_to_vacant_not_to_a_key_bearing_state() {
        let mut t = table();
        let token = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        assert_eq!(t.restore(token), Ok(()));
        assert_eq!(t.slots[base().endpoint_index], WaiterOwnershipSlot::Vacant);
        assert_eq!(t.view(&base()), WaiterOwnershipView::Vacant);
        let second = t
            .claim(base(), WaiterOwner::LegacyDelivery)
            .expect("reclaim");
        assert_ne!(second.claim_generation(), token.claim_generation());
    }

    /// **The claim generation alone must reject a stale token.** Re-claimed by the *same* owner
    /// for the *same* incarnation, so neither the owner comparison nor the key comparison can
    /// mask it — only `claim_generation` distinguishes the old token from the live one.
    #[test]
    fn a_stale_token_is_rejected_even_when_the_same_owner_reclaims() {
        let mut t = table();
        let first = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        assert_eq!(t.restore(first), Ok(()));
        let second = t
            .claim(base(), WaiterOwner::DirectRequest)
            .expect("same owner reclaims the same incarnation");
        assert_ne!(second.claim_generation(), first.claim_generation());
        assert_eq!(second.key(), first.key());
        for (what, r) in [
            ("consume", t.consume(first)),
            ("restore", t.restore(first)),
            ("cancel", t.cancel(first)),
        ] {
            assert_eq!(
                r,
                Err(SettleError::StaleClaimGeneration),
                "{what} with a stale token must be refused on the claim generation alone"
            );
        }
        assert_eq!(
            t.view(&base()),
            WaiterOwnershipView::Claimed {
                owner: WaiterOwner::DirectRequest
            },
            "and the live claim is untouched"
        );
        assert_eq!(t.consume(second), Ok(()), "the live token still works");
    }

    // ── C. Generation safety ────────────────────────────────────────────────────────────────

    #[test]
    fn claim_generations_start_at_one_and_never_repeat() {
        let mut t = table();
        let first = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        assert_eq!(first.claim_generation(), 1, "zero is never issued");
        assert_eq!(t.restore(first), Ok(()));
        let second = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        assert_eq!(second.claim_generation(), 2);
    }

    /// Exhaustion is typed and fail-closed. A wrapping counter would make an ancient token valid
    /// again; `checked_add` refuses instead, leaving the slot and the counter untouched.
    #[test]
    fn a_saturated_claim_generation_fails_closed_without_mutating_the_slot() {
        let mut t = table();
        t.next_claim_generation = u64::MAX;
        assert_eq!(
            t.claim(base(), WaiterOwner::DirectRequest),
            Err(ClaimError::ClaimGenerationExhausted)
        );
        assert_eq!(t.slots[base().endpoint_index], WaiterOwnershipSlot::Vacant);
        assert_eq!(t.claimed_count(), 0);
        assert_eq!(
            t.next_claim_generation,
            u64::MAX,
            "the counter never wraps past its last usable value"
        );
        // It stays closed: retrying does not eventually succeed.
        assert_eq!(
            t.claim(base(), WaiterOwner::Teardown),
            Err(ClaimError::ClaimGenerationExhausted)
        );
    }

    /// The last usable generation is still issued, and only then does the table close.
    #[test]
    fn the_final_usable_generation_is_issued_before_exhaustion() {
        let mut t = table();
        t.next_claim_generation = u64::MAX - 1;
        let token = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        assert_eq!(token.claim_generation(), u64::MAX - 1);
        assert_eq!(t.restore(token), Ok(()));
        assert_eq!(
            t.claim(base(), WaiterOwner::DirectRequest),
            Err(ClaimError::ClaimGenerationExhausted)
        );
    }

    // ── B. The IpcSubsystem surface is the only usable one, and it works ────────────────────

    /// Drives the whole lifecycle through the **public** surface — `&mut IpcSubsystem` handed out
    /// by the ipc rank-3 seam — rather than the module-private table methods. This is the exact
    /// shape a future owner will use: claim under the guard, carry the `Copy` token out, settle
    /// under a later guard.
    #[test]
    fn the_ipc_subsystem_surface_drives_a_full_claim_lifecycle() {
        let mut state = crate::kernel::boot::Bootstrap::init().expect("init");
        let k = base();

        // Claim under one rank-3 acquisition…
        let token = state
            .with_ipc_state_mut(|ipc| {
                assert_eq!(ipc.waiter_ownership_view(&k), WaiterOwnershipView::Vacant);
                ipc.waiter_ownership_claim(k, WaiterOwner::DirectRequest)
            })
            .expect("claim");
        // …the token survives the guard being dropped, and reports exactly the three facts a
        // future owner needs — never the claim generation.
        assert_eq!(token.owner(), WaiterOwner::DirectRequest);
        assert_eq!(token.endpoint_index(), k.endpoint_index);
        assert_eq!(token.waiter(), k.waiter);

        // A competitor loses under a *later* acquisition.
        state.with_ipc_state_mut(|ipc| {
            assert_eq!(
                ipc.waiter_ownership_view(&k),
                WaiterOwnershipView::Claimed {
                    owner: WaiterOwner::DirectRequest
                }
            );
            assert_eq!(
                ipc.waiter_ownership_claim(k, WaiterOwner::OrdinaryTimeout),
                Err(ClaimError::AlreadyClaimed {
                    by: WaiterOwner::DirectRequest
                })
            );
            assert_eq!(ipc.waiter_ownership_claimed_count(), 1);
        });

        // Restore, re-claim, then consume and cancel through the same surface.
        state.with_ipc_state_mut(|ipc| {
            assert_eq!(ipc.waiter_ownership_restore(token), Ok(()));
            assert_eq!(ipc.waiter_ownership_claimed_count(), 0);
            let again = ipc
                .waiter_ownership_claim(k, WaiterOwner::DirectReply)
                .expect("reclaim");
            assert_eq!(
                ipc.waiter_ownership_restore(token),
                Err(SettleError::ForeignOwner {
                    by: WaiterOwner::DirectReply
                }),
                "the spent token cannot settle the new claim"
            );
            assert_eq!(ipc.waiter_ownership_consume(again), Ok(()));
            assert_eq!(ipc.waiter_ownership_view(&k), WaiterOwnershipView::Consumed);

            let next = WaiterKey {
                wait_generation: k.wait_generation + 1,
                ..k
            };
            let t3 = ipc
                .waiter_ownership_claim(next, WaiterOwner::Teardown)
                .expect("a later incarnation takes the slot over");
            assert_eq!(ipc.waiter_ownership_cancel(t3), Ok(()));
            assert_eq!(
                ipc.waiter_ownership_view(&next),
                WaiterOwnershipView::Cancelled
            );
        });
    }

    /// A freshly booted kernel holds no claims: the primitive is helper-only, so nothing on any
    /// live path arms it.
    #[test]
    fn a_booted_kernel_holds_no_ownership_claims() {
        let mut state = crate::kernel::boot::Bootstrap::init().expect("init");
        state.with_ipc_state_mut(|ipc| {
            assert_eq!(ipc.waiter_ownership_claimed_count(), 0);
        });
    }

    // ── Scope: the primitive mutates nothing else ───────────────────────────────────────────

    /// It is a pure state machine over owned data: it names no reply record, reverse link,
    /// capability, TCB or scheduler type, and acquires no lock — so it cannot nest a task or
    /// scheduler lock beneath the ipc rank-3 guard the caller supplies.
    #[test]
    fn the_primitive_touches_no_other_subsystem_and_takes_no_lock() {
        const SRC: &str = include_str!("waiter_ownership.rs");
        let code: alloc::string::String = SRC
            .lines()
            .take_while(|l| !l.starts_with("#[cfg(test)]"))
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("//") && !t.starts_with("///") && !t.starts_with("//!")
            })
            .collect::<alloc::vec::Vec<_>>()
            .join("\n");
        for forbidden in [
            "with_ipc_state_mut",
            "with_tcbs_mut",
            "with_scheduler",
            "with_cpu",
            "lock()",
            "SpinLock",
            "reply_cap",
            "ReplyCapRecord",
            "server_reply_link",
            "ThreadControlBlock",
            "TaskStatus",
            "enqueue",
            "Scheduler",
            "CapId",
            "KernelState",
        ] {
            assert!(
                !code.contains(forbidden),
                "the primitive must not name `{forbidden}`:\n{code}"
            );
        }
    }
}
