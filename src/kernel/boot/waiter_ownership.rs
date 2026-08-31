// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199D-WA2A-R2 — the generation-bearing endpoint-waiter ownership primitive.
//!
//! # Why
//!
//! `WAITER_OWNERSHIP_EXCLUSIVE=no`. The endpoint receive-waiter table is the *de facto* arbiter
//! between the paths that can wake a `Blocked(EndpointReceive)` task — but only for the paths
//! that happen to consult it, and the independent status-transition census in
//! `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.32 shows that most of them do not. That is why the x86
//! direct production default is OFF.
//!
//! This module is the single typed primitive every one of those paths is routed through.
//!
//! # Stage 199D-WA3C2 — WIRED, and authoritative
//!
//! WA2A-R2 shipped this primitive with zero production callers. WA3C2 wires it, and the wiring
//! is **structural rather than remembered**: the ownership slot is reconciled inside the two
//! functions that are already the single publish point and the single clear point of the
//! authoritative waiter table — `IpcSubsystem::publish_endpoint_waiter` and
//! `IpcSubsystem::remove_endpoint_waiter_at`. That is the same argument
//! `release_direct_ack_lease` and the waiter census already make in those two bodies: a new
//! removal or publication path inherits ownership for free, and cannot forget it.
//!
//! Consequently ARM and RETIRE have production callers on **every** waiter lifecycle edge —
//! publication, last-receiver-wins replacement, legacy delivery, ordinary and reply timeout,
//! notification wake, endpoint destruction and task death — without a single scattered call.
//! CLAIM, CONSUME, CANCEL and RESTORE are the *explicit* half, used where an owner must hold
//! the incarnation across a lock release: the off-lock NR6/NR7 direct transactions and the
//! shared-region finalizer claim through
//! [`SharedKernel::sr_claim_endpoint_waiter_split`](crate::runtime::SharedKernel), and the
//! displaced-receiver transition below.
//!
//! ## What makes it authoritative — and why direct production can now default on
//!
//! A live claim outlives the rank-3 guard, and the direct transactions *remove* the waiter as
//! part of claiming it. So between CLAIM and settle there is a window in which the waiter table
//! says "nobody is waiting here" while an in-flight transaction is still going to deliver to,
//! and wake, that exact incarnation. Before WA3C2 nothing recorded that window, so a second
//! receiver could publish into the endpoint and become the target of a wake it never earned.
//!
//! `arm_current` refuses an occupied slot, and WA3C2 does **not** weaken it. The claim therefore
//! keeps the slot occupied for the whole window, and a publication that would take the endpoint
//! out from under a live claim is refused with
//! [`PublishWaiterOutcome::WaiterOwnershipBusy`](crate::kernel::recv_waiter_split::PublishWaiterOutcome)
//! — the receiver unwinds and retries instead of silently displacing an owned incarnation. That
//! is the missing exclusivity `WAITER_OWNERSHIP_EXCLUSIVE=no` named, and it is what the x86_64
//! direct production default rests on.
//!
//! ## The displaced-receiver transition
//!
//! Last-receiver-wins is PRESERVED. What WA3C2 removes is its *ownerless* form: the displaced
//! record used to be logged and dropped, leaving the old receiver blocked with no waiter and no
//! owner. The replacement now performs one explicit transition —
//! CLAIM([`WaiterOwner::Teardown`]) → CANCEL → RETIRE → ARM(new) — so the departing incarnation
//! reaches a terminal ownership state under a named owner before the arriving one is armed, and
//! a displacement that would race a live claim is refused rather than performed.
//!
//! # An armed current incarnation, not a claim-any-key table
//!
//! The table is **endpoint-indexed**: one slot per endpoint index, with
//! `WAITER_OWNERSHIP_SLOTS` derived from [`ENDPOINT_WAITER_SLOTS`] and pinned by a compile-time
//! assertion so the two can never drift. At most one endpoint receive-waiter can exist per
//! endpoint index, so at most one ownership claim can either — the same structural argument
//! [`crate::kernel::direct_ack_store`] makes about acknowledgement leases.
//!
//! A slot names **the current incarnation and no other**:
//!
//! ```text
//!   Vacant ──arm_current(k)──► Available{k} ──claim(k,owner)──► Claimed{k,owner,gen}
//!     ▲                            ▲                              │
//!     │                            └──────── restore(token) ──────┤
//!     │                                                           │
//!     └── retire_current(k) ◄── Consumed{k} / Cancelled{k} ◄── consume/cancel(token)
//! ```
//!
//! `claim` **never installs a key**. It succeeds only from `Available` holding that exact key,
//! so a delayed request for an older incarnation cannot move the slot backward:
//!
//! ```text
//!   arm A, claim A, consume A, retire A, arm B, claim B, consume B, then a delayed claim A
//!   → NoSuchCurrentIncarnation (B holds the slot), never a fresh token for A
//! ```
//!
//! WA2A-R1 accepted that delayed claim, because it treated "key differs from the terminal key"
//! as "a newer incarnation is taking over". Chronology is not derivable from a key, so the
//! current incarnation is now stated explicitly by whoever publishes the waiter.
//!
//! ## Bounded and leak-free, with an obligation
//!
//! No historical-key store returns: the slot holds exactly one key at a time, and
//! `retire_current` returns it to `Vacant`. The obligation that buys this is explicit —
//! **a terminal slot blocks its endpoint index until it is retired.** That is fail-closed (a
//! stale incarnation can never be claimed) but it is a liveness duty the wiring increment
//! inherits: whatever clears the authoritative receive-waiter must also retire the slot, under
//! the same ipc rank-3 acquisition.
//!
//! # Lock discipline and encapsulation
//!
//! [`WaiterOwnershipTable`] is a field of [`IpcSubsystem`], so reaching it requires the ipc
//! rank-3 guard the caller already holds, and no task, scheduler, capability, VM or broad-state
//! API can name it. Every claim/settle/arm/retire method on the table is **module-private**; the
//! intended cross-module surface is the typed `IpcSubsystem::waiter_ownership_*` methods below.
//!
//! That is **rank-3 co-location plus source-guarded encapsulation — not complete
//! type-system-enforced inertness.** The field and [`WaiterOwnershipTable::vacant`] are visible
//! within `crate::kernel::boot` (`pub(in …)` requires an ancestor module, and `boot` is the
//! nearest one shared by `defs`, which declares the field, and this module, which must reach
//! it). A boot sibling therefore *could* replace the whole table by assignment,
//! `mem::replace`/`swap` or a raw pointer write even though it cannot call a single method on
//! it. `no_boot_sibling_can_replace_or_borrow_the_ownership_table` rejects exactly those forms;
//! the guarantee is enforced by guard, and this comment says so rather than implying the type
//! system does it.
//!
//! The module acquires nothing and calls into no other domain, so it cannot nest task(2) or
//! scheduler(1) beneath ipc(3). [`WaiterClaimToken`] is opaque and `Copy`, so it survives the
//! guard being released.

// Stage 199D-WA3C2: the HELPER-ONLY dead-code fence is GONE. Every item below is reachable from
// a production caller on a freestanding build, and
// `the_ownership_primitive_has_production_callers` is what enforces that it stays so.

use super::ENDPOINT_WAITER_SLOTS;
use super::defs::{IpcSubsystem, ReceiverWaiterIdentity};

/// One ownership slot per endpoint index. **Derived**, never a second numeric literal — the
/// assertion below makes an accidental redefinition a compile error rather than a silent
/// capacity split.
pub(crate) const WAITER_OWNERSHIP_SLOTS: usize = ENDPOINT_WAITER_SLOTS;

const _: () = assert!(WAITER_OWNERSHIP_SLOTS == ENDPOINT_WAITER_SLOTS);

/// Which subsystem owns a claim.
///
/// Stage 199D-WA3C2: `DirectRequest`, `DirectReply`, `LegacyDelivery` and `Teardown` are the
/// four that mint tokens in production — the paths that must hold an incarnation across a lock
/// release. `OrdinaryTimeout` and `Notification` retire through the structural edge instead of
/// claiming, because both settle inside the single rank-3 section that clears the waiter and so
/// have no window to protect; they are named here because a claim-bearing variant of either is
/// a same-shape change that must not have to re-derive the enum.
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
    /// Task teardown: exit, mark-dead, reap, restart — and the displaced-receiver transition,
    /// which is a teardown of the departing incarnation's wait performed by its replacement.
    Teardown,
}

impl WaiterOwner {
    /// Stable lowercase name for telemetry. Exhaustive by construction — no wildcard arm, so a
    /// new owner cannot inherit another's label.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::DirectRequest => "direct_request",
            Self::DirectReply => "direct_reply",
            Self::OrdinaryTimeout => "ordinary_timeout",
            Self::LegacyDelivery => "legacy_delivery",
            Self::Notification => "notification",
            Self::Teardown => "teardown",
        }
    }
}

/// Stage 199D-WA3C2 — what the structural reconciliation performed for one waiter-table edge.
///
/// The two funnels do not return `Result`, because "the owner still holds this incarnation" is
/// not an error: it is the normal shape of the off-lock direct transactions, and the whole
/// reason the table exists. Reporting it as a distinct variant is what lets the publish policy
/// refuse rather than the removal path guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaiterOwnershipTransition {
    /// The ownership slot now matches the waiter table exactly.
    Synchronized,
    /// The slot already agreed; nothing needed to move.
    AlreadyConsistent,
    /// A departing incarnation is under a LIVE claim. The slot was deliberately **not**
    /// retired: its owner is mid-transaction and settles it. This is the expected report on
    /// every direct-transaction claim.
    HeldByOwner { owner: WaiterOwner },
    /// The arriving incarnation could not be armed because the slot names another one.
    ArmRefused { holding: WaiterSlotKind },
    /// The slot names a different incarnation than the edge being reconciled, so nothing about
    /// this key is retirable. Left untouched.
    ForeignIncarnation { holding: WaiterSlotKind },
    /// The endpoint index has no ownership slot (out of range).
    NoSlot,
}

impl WaiterOwnershipTransition {
    /// Whether the ownership slot is free of any claim or record that would block this edge —
    /// i.e. whether a caller may proceed to publish over it.
    pub(crate) fn permits_publication(self) -> bool {
        matches!(self, Self::Synchronized | Self::AlreadyConsistent)
    }

    /// Stable lowercase reason for telemetry.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Synchronized => "synchronized",
            Self::AlreadyConsistent => "already_consistent",
            Self::HeldByOwner { .. } => "held_by_owner",
            Self::ArmRefused { .. } => "arm_refused",
            Self::ForeignIncarnation { .. } => "foreign_incarnation",
            Self::NoSlot => "no_slot",
        }
    }
}

/// The exact incarnation a slot is armed for. All four dimensions are load-bearing:
///
/// * `endpoint_index` **and** `endpoint_generation` — a destroyed-and-recreated endpoint reusing
///   the slot is a different key;
/// * `waiter` (`{tid, asid}`) — a replacement task reusing the numeric TID is a different key;
/// * `wait_generation` — the task's blocked-receive generation, so a task that unblocked, ran and
///   reblocked on the same endpoint is a different key.
///
/// A key states *which* incarnation, never *when*. Chronology comes from
/// [`WaiterOwnershipTable::arm_current`], which is why `claim` may not install one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WaiterKey {
    pub(crate) endpoint_index: usize,
    pub(crate) endpoint_generation: u64,
    pub(crate) waiter: ReceiverWaiterIdentity,
    pub(crate) wait_generation: u64,
}

/// What a slot holds, with no key and no claim generation. Used to describe a slot the caller is
/// **not** asking about, and to report terminal states back to the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaiterSlotKind {
    /// Armed for some incarnation, unclaimed.
    Available,
    /// Some owner holds a live claim.
    Claimed,
    /// Some incarnation completed and has not been retired.
    Consumed,
    /// Some incarnation was abandoned and has not been retired.
    Cancelled,
}

/// The answer to "what is the state of *this* incarnation?".
///
/// **The exact contract** (WA2B-CENSUS corrected the earlier wording):
///
/// * [`WaiterOwnershipView::Available`] means the exact incarnation is **armed and unclaimed**;
/// * it is the **only** slot state structurally eligible for [`WaiterOwnershipTable::claim`] —
///   every other view necessarily rejects a claim;
/// * `Available` is *not* a promise that a claim succeeds: it may still fail closed with
///   [`ClaimError::ClaimGenerationExhausted`], which mutates nothing and leaves the slot
///   `Available`.
///
/// A slot held by a **different** incarnation reports
/// [`WaiterOwnershipView::ForeignIncarnation`], never `Vacant` — WA2A-R1 reported `Vacant` there
/// and for an out-of-range index, both of which implied a claim would be accepted.
///
/// Deliberately carries no `claim_generation` and no counter state: knowing either would let a
/// caller reconstruct authority it never earned, or predict exhaustion it has no business
/// predicting. Only a successful claim mints a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaiterOwnershipView {
    /// The key names an endpoint index the table does not have.
    EndpointIndexOutOfRange,
    /// The slot is genuinely empty. Nothing is armed, so nothing is claimable **yet**: a
    /// `claim` here fails with [`ClaimError::NoCurrentWaiter`] until someone arms it.
    Vacant,
    /// Armed for exactly this incarnation and unclaimed: the only state structurally eligible
    /// for a claim. A claim from here may still fail closed on
    /// [`ClaimError::ClaimGenerationExhausted`].
    Available,
    /// This exact incarnation is owned.
    Claimed { owner: WaiterOwner },
    /// This exact incarnation completed. Terminal until retired.
    Consumed,
    /// This exact incarnation was abandoned. Terminal until retired.
    Cancelled,
    /// The slot belongs to a **different** incarnation of this endpoint index. Nothing about
    /// the requested key is claimable, armable or retirable.
    ForeignIncarnation { holding: WaiterSlotKind },
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

    /// Stage 199D-WA3C2, **test-only**: a token no table ever minted.
    ///
    /// It exists so a test can build a deliberately STALE
    /// [`WaiterClaim`](crate::runtime::WaiterClaim) — one naming a recycled endpoint
    /// generation or a vanished incarnation — and prove the settle paths refuse it. Every
    /// settle validates the key, the owner AND the claim generation against the live slot, so
    /// this token is refused by construction; that refusal is exactly what the tests assert.
    ///
    /// `#[cfg(test)]` is the whole guarantee: no freestanding build compiles a way to make one.
    #[cfg(test)]
    pub(crate) fn for_test(key: WaiterKey, owner: WaiterOwner) -> Self {
        Self {
            key,
            owner,
            claim_generation: 0,
        }
    }
}

/// Why arming the current incarnation was refused. Fail-closed: an occupied slot is never
/// overwritten, whatever it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArmError {
    /// The key names an endpoint index the table does not have.
    EndpointIndexOutOfRange { index: usize },
    /// The slot already names an incarnation — armed, claimed, or terminal-and-unretired.
    /// Whoever holds it must retire it first; arming never displaces.
    SlotOccupied { holding: WaiterSlotKind },
}

/// Why a claim was refused. Every variant is fail-closed: nothing is armed, evicted, overwritten
/// or stamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimError {
    /// The key names an endpoint index the table does not have. Fail closed rather than wrap or
    /// panic — an out-of-range index is a caller bug, not a capacity event.
    EndpointIndexOutOfRange { index: usize },
    /// Nothing is armed for this endpoint index. `claim` never installs a key, so there is
    /// nothing to take.
    NoCurrentWaiter,
    /// The slot is armed for a **different** incarnation. This is what refuses a delayed request
    /// for an older incarnation after a newer one took the slot.
    NoSuchCurrentIncarnation { holding: WaiterSlotKind },
    /// This exact incarnation already has a live owner. Never evicted.
    AlreadyClaimed { by: WaiterOwner },
    /// Terminal — this exact incarnation was already delivered and not yet retired.
    Consumed,
    /// Terminal — this exact incarnation was already abandoned and not yet retired.
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
    /// The slot does not hold this exact key — vacant, or armed for a later incarnation. A
    /// recycled endpoint generation, a replacement task and a re-blocked task all land here,
    /// which is precisely what stops a stale restore.
    NoSuchClaim,
    /// The slot is claimed for this exact incarnation, but by a different owner.
    ForeignOwner { by: WaiterOwner },
    /// The right owner and the right incarnation, but an older claim than the live one: a stale
    /// token. The live generation is deliberately **not** reported.
    StaleClaimGeneration,
    /// The slot holds this exact key, but not as a live claim.
    NotClaimed { state: WaiterSlotKind },
}

/// Why retiring the current incarnation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetireError {
    /// The key names an endpoint index the table does not have.
    EndpointIndexOutOfRange { index: usize },
    /// Nothing is armed for this endpoint index.
    NotArmed,
    /// The slot belongs to a different incarnation. A stale retire can never clear a newer one.
    NoSuchCurrentIncarnation { holding: WaiterSlotKind },
    /// The exact incarnation is live. A claim outstanding with some owner is never retired out
    /// from under it — that owner must settle first.
    LiveClaim { by: WaiterOwner },
}

/// One endpoint index's ownership state. At most one incarnation at a time, always named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaiterOwnershipSlot {
    /// Nothing is armed for this endpoint index.
    Vacant,
    /// Armed for exactly this incarnation and unclaimed.
    Available { key: WaiterKey },
    /// A live claim. Only the exact `{key, owner, claim_generation}` may settle it.
    Claimed {
        key: WaiterKey,
        owner: WaiterOwner,
        claim_generation: u64,
    },
    /// The named incarnation completed. Terminal until `retire_current`.
    Consumed { key: WaiterKey },
    /// The named incarnation was abandoned. Terminal until `retire_current`.
    Cancelled { key: WaiterKey },
}

impl WaiterOwnershipSlot {
    /// The incarnation this slot names, if it names one.
    fn key(&self) -> Option<&WaiterKey> {
        match self {
            Self::Vacant => None,
            Self::Available { key }
            | Self::Claimed { key, .. }
            | Self::Consumed { key }
            | Self::Cancelled { key } => Some(key),
        }
    }

    /// What the slot holds, without saying which incarnation.
    fn kind(&self) -> Option<WaiterSlotKind> {
        match self {
            Self::Vacant => None,
            Self::Available { .. } => Some(WaiterSlotKind::Available),
            Self::Claimed { .. } => Some(WaiterSlotKind::Claimed),
            Self::Consumed { .. } => Some(WaiterSlotKind::Consumed),
            Self::Cancelled { .. } => Some(WaiterSlotKind::Cancelled),
        }
    }
}

/// The endpoint-indexed ownership table. Co-located with the ipc domain: it holds no lock of its
/// own, and every entry point is reached through the caller's ipc rank-3 guard.
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
    /// zero initialization. Visibility stops at the boot domain; a guard, not the type system,
    /// is what keeps this the only construction site.
    pub(in crate::kernel::boot) const fn vacant() -> Self {
        Self {
            slots: [WaiterOwnershipSlot::Vacant; WAITER_OWNERSHIP_SLOTS],
            next_claim_generation: 1,
        }
    }

    fn slot(&self, index: usize) -> Option<&WaiterOwnershipSlot> {
        self.slots.get(index)
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

    /// Declare `key` the **current** incarnation of its endpoint index.
    ///
    /// This is the only way a key ever enters the table, and it is what makes chronology
    /// explicit: the eventual authoritative waiter-publication path arms here, under the same
    /// ipc rank-3 acquisition that installs the receive-waiter. An occupied slot — armed,
    /// claimed or terminal-and-unretired — is never displaced.
    fn arm_current(&mut self, key: WaiterKey) -> Result<(), ArmError> {
        let index = key.endpoint_index;
        let slot = self
            .slot(index)
            .ok_or(ArmError::EndpointIndexOutOfRange { index })?;
        if let Some(holding) = slot.kind() {
            return Err(ArmError::SlotOccupied { holding });
        }
        self.slots[index] = WaiterOwnershipSlot::Available { key };
        Ok(())
    }

    /// Take ownership of the current incarnation. **Never installs or replaces a key**: it
    /// succeeds only from `Available` holding exactly this key.
    fn claim(
        &mut self,
        key: WaiterKey,
        owner: WaiterOwner,
    ) -> Result<WaiterClaimToken, ClaimError> {
        let index = key.endpoint_index;
        let slot = self
            .slot(index)
            .ok_or(ClaimError::EndpointIndexOutOfRange { index })?;
        let Some(holding) = slot.kind() else {
            return Err(ClaimError::NoCurrentWaiter);
        };
        if slot.key() != Some(&key) {
            // A different incarnation holds the slot. This is the reverse-chronology refusal:
            // a delayed request for an older incarnation gets no token, whatever its key says.
            return Err(ClaimError::NoSuchCurrentIncarnation { holding });
        }
        match slot {
            WaiterOwnershipSlot::Available { .. } => {}
            WaiterOwnershipSlot::Claimed { owner: by, .. } => {
                return Err(ClaimError::AlreadyClaimed { by: *by });
            }
            WaiterOwnershipSlot::Consumed { .. } => return Err(ClaimError::Consumed),
            WaiterOwnershipSlot::Cancelled { .. } => return Err(ClaimError::Cancelled),
            WaiterOwnershipSlot::Vacant => unreachable!("kind() already returned Some"),
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
            .slot(index)
            .ok_or(SettleError::EndpointIndexOutOfRange { index })?;
        if slot.key() != Some(&token.key) {
            // Vacant, or armed for a later incarnation: this token was superseded.
            return Err(SettleError::NoSuchClaim);
        }
        match slot {
            WaiterOwnershipSlot::Claimed {
                owner,
                claim_generation,
                ..
            } => {
                if *owner != token.owner {
                    return Err(SettleError::ForeignOwner { by: *owner });
                }
                if *claim_generation != token.claim_generation {
                    return Err(SettleError::StaleClaimGeneration);
                }
                Ok(index)
            }
            other => Err(SettleError::NotClaimed {
                state: other
                    .kind()
                    .expect("key() matched, so the slot is occupied"),
            }),
        }
    }

    /// Terminal success: the owner delivered. The incarnation stays named until it is retired,
    /// so nothing can re-arm the slot behind it.
    fn consume(&mut self, token: WaiterClaimToken) -> Result<(), SettleError> {
        let index = self.validate(&token)?;
        self.slots[index] = WaiterOwnershipSlot::Consumed { key: token.key };
        Ok(())
    }

    /// Non-terminal rollback: the owner did not deliver, so the **same** incarnation becomes
    /// claimable again. It does not return to `Vacant` — the waiter is still published, and
    /// dropping the key would let an older delayed request arm the slot.
    fn restore(&mut self, token: WaiterClaimToken) -> Result<(), SettleError> {
        let index = self.validate(&token)?;
        self.slots[index] = WaiterOwnershipSlot::Available { key: token.key };
        Ok(())
    }

    /// Terminal abandonment: the incarnation is dead (task exited, endpoint destroyed). Named
    /// until retired, exactly like `consume`.
    fn cancel(&mut self, token: WaiterClaimToken) -> Result<(), SettleError> {
        let index = self.validate(&token)?;
        self.slots[index] = WaiterOwnershipSlot::Cancelled { key: token.key };
        Ok(())
    }

    /// Release the slot for the **exact** incarnation named. The counterpart of `arm_current`:
    /// the eventual publication path retires here when it clears the authoritative
    /// receive-waiter, which is what keeps the table leak-free.
    ///
    /// A live claim is never retired out from under its owner, and a stale retire naming an
    /// older incarnation can never clear a newer one.
    fn retire_current(&mut self, expected: WaiterKey) -> Result<(), RetireError> {
        let index = expected.endpoint_index;
        let slot = self
            .slot(index)
            .ok_or(RetireError::EndpointIndexOutOfRange { index })?;
        let Some(holding) = slot.kind() else {
            return Err(RetireError::NotArmed);
        };
        if slot.key() != Some(&expected) {
            return Err(RetireError::NoSuchCurrentIncarnation { holding });
        }
        if let WaiterOwnershipSlot::Claimed { owner, .. } = slot {
            return Err(RetireError::LiveClaim { by: *owner });
        }
        self.slots[index] = WaiterOwnershipSlot::Vacant;
        Ok(())
    }

    /// Stage 199D-WA3C2 — retire a slot whose incarnation belongs to a **superseded endpoint
    /// generation**, returning what it was holding.
    ///
    /// This is the one retire that does not name an exact key, and it is sound for a reason no
    /// other retire can claim: the slot's `endpoint_generation` is strictly older than the
    /// endpoint's current one, so the endpoint it named has been destroyed and recreated. That
    /// incarnation can never be claimed, restored or delivered to again — every claim path
    /// requires a generation match — so the slot holds nothing but a permanent obstruction.
    ///
    /// Without it a destroyed-and-recreated endpoint could wedge its index forever: the only
    /// key that could retire the slot is one no caller is able to reconstruct once the
    /// generation has moved on.
    ///
    /// A LIVE claim is still refused. An owner mid-transaction settles its own claim, and its
    /// settle paths already fail closed on the generation change.
    fn retire_superseded_generation(
        &mut self,
        index: usize,
        current_generation: u64,
    ) -> Option<WaiterSlotKind> {
        let slot = self.slot(index)?;
        let holding = slot.kind()?;
        let key = slot.key()?;
        if key.endpoint_generation >= current_generation {
            return None;
        }
        if matches!(slot, WaiterOwnershipSlot::Claimed { .. }) {
            return None;
        }
        self.slots[index] = WaiterOwnershipSlot::Vacant;
        Some(holding)
    }

    /// The state of **this** incarnation. Never reports `Vacant` for a slot another incarnation
    /// holds, and never reports `Vacant` for an index the table does not have.
    fn view(&self, key: &WaiterKey) -> WaiterOwnershipView {
        let Some(slot) = self.slot(key.endpoint_index) else {
            return WaiterOwnershipView::EndpointIndexOutOfRange;
        };
        let Some(holding) = slot.kind() else {
            return WaiterOwnershipView::Vacant;
        };
        if slot.key() != Some(key) {
            return WaiterOwnershipView::ForeignIncarnation { holding };
        }
        match slot {
            WaiterOwnershipSlot::Available { .. } => WaiterOwnershipView::Available,
            WaiterOwnershipSlot::Claimed { owner, .. } => {
                WaiterOwnershipView::Claimed { owner: *owner }
            }
            WaiterOwnershipSlot::Consumed { .. } => WaiterOwnershipView::Consumed,
            WaiterOwnershipSlot::Cancelled { .. } => WaiterOwnershipView::Cancelled,
            WaiterOwnershipSlot::Vacant => unreachable!("kind() already returned Some"),
        }
    }

    /// Live (claimed, non-terminal) entries. Diagnostic only.
    fn claimed_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| matches!(s, WaiterOwnershipSlot::Claimed { .. }))
            .count()
    }

    /// Slots naming any incarnation at all — armed, claimed or terminal-and-unretired.
    /// Diagnostic only; it is what a leak would show up in.
    fn occupied_count(&self) -> usize {
        self.slots.iter().filter(|s| s.kind().is_some()).count()
    }
}

/// The intended cross-module surface of the primitive. Every one of these requires
/// `&mut IpcSubsystem` (or `&IpcSubsystem`), which only the ipc rank-3 guard hands out, and the
/// table's own methods are private to this module — so ownership cannot be *operated* from the
/// task, scheduler, capability, VM or broad-state APIs at all. Wholesale replacement of the
/// table by a boot sibling is refused by guard rather than by the type system; see the module
/// doc.
impl IpcSubsystem {
    pub(crate) fn waiter_ownership_arm_current(&mut self, key: WaiterKey) -> Result<(), ArmError> {
        self.waiter_ownership.arm_current(key)
    }

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

    pub(crate) fn waiter_ownership_retire_current(
        &mut self,
        expected: WaiterKey,
    ) -> Result<(), RetireError> {
        self.waiter_ownership.retire_current(expected)
    }

    pub(crate) fn waiter_ownership_view(&self, key: &WaiterKey) -> WaiterOwnershipView {
        self.waiter_ownership.view(key)
    }

    pub(crate) fn waiter_ownership_claimed_count(&self) -> usize {
        self.waiter_ownership.claimed_count()
    }

    pub(crate) fn waiter_ownership_occupied_count(&self) -> usize {
        self.waiter_ownership.occupied_count()
    }

    // ── Stage 199D-WA3C2: the structural reconciliation surface ────────────────────────────
    //
    // These are what `publish_endpoint_waiter` and `remove_endpoint_waiter_at` call, and they
    // are the reason ownership coverage is structural. They live here rather than in
    // `ipc_state` so that every decision about the state machine stays in the module that owns
    // it — `ipc_state` supplies the edge, this module decides what it means.

    /// The exact [`WaiterKey`] for a record resident at `idx`, or `None` when the index has no
    /// endpoint generation (out of range).
    ///
    /// The endpoint GENERATION is read from the live table rather than passed in, so no caller
    /// can name an incarnation of an endpoint that is not the current one.
    pub(crate) fn waiter_ownership_key_for(
        &self,
        idx: usize,
        record: super::defs::EndpointWaiterRecord,
    ) -> Option<WaiterKey> {
        let generation = *self.endpoint_generations.get(idx)?;
        Some(WaiterKey {
            endpoint_index: idx,
            endpoint_generation: generation,
            waiter: record.receiver,
            wait_generation: record.wait_generation,
        })
    }

    /// Reconcile the ownership slot with a **departing** incarnation: the record the waiter
    /// table has just removed, or is about to displace.
    ///
    /// Retires the slot when it names exactly this incarnation and no owner holds it. A LIVE
    /// claim is deliberately left alone and reported as
    /// [`WaiterOwnershipTransition::HeldByOwner`]: the direct transactions remove the waiter
    /// *as part of* claiming it, so a retire here would destroy the very window the claim
    /// exists to hold — and `retire_current` would refuse it anyway.
    pub(crate) fn waiter_ownership_reconcile_departure(
        &mut self,
        idx: usize,
        record: super::defs::EndpointWaiterRecord,
    ) -> WaiterOwnershipTransition {
        let Some(key) = self.waiter_ownership_key_for(idx, record) else {
            return WaiterOwnershipTransition::NoSlot;
        };
        match self.waiter_ownership_view(&key) {
            WaiterOwnershipView::EndpointIndexOutOfRange => WaiterOwnershipTransition::NoSlot,
            WaiterOwnershipView::Vacant => WaiterOwnershipTransition::AlreadyConsistent,
            WaiterOwnershipView::Claimed { owner } => {
                WaiterOwnershipTransition::HeldByOwner { owner }
            }
            WaiterOwnershipView::ForeignIncarnation { holding } => {
                WaiterOwnershipTransition::ForeignIncarnation { holding }
            }
            // Armed-and-unclaimed, or terminal-and-unretired for exactly this incarnation:
            // the waiter is leaving, so the slot must return to `Vacant`. `retire_current`
            // re-checks the key itself, so this cannot clear a newer incarnation.
            WaiterOwnershipView::Available
            | WaiterOwnershipView::Consumed
            | WaiterOwnershipView::Cancelled => match self.waiter_ownership_retire_current(key) {
                Ok(()) => WaiterOwnershipTransition::Synchronized,
                Err(RetireError::EndpointIndexOutOfRange { .. }) => {
                    WaiterOwnershipTransition::NoSlot
                }
                Err(RetireError::NotArmed) => WaiterOwnershipTransition::AlreadyConsistent,
                Err(RetireError::NoSuchCurrentIncarnation { holding }) => {
                    WaiterOwnershipTransition::ForeignIncarnation { holding }
                }
                Err(RetireError::LiveClaim { by }) => {
                    WaiterOwnershipTransition::HeldByOwner { owner: by }
                }
            },
        }
    }

    /// Reconcile the ownership slot with an **arriving** incarnation: the record the waiter
    /// table has just published.
    ///
    /// `arm_current` is NOT weakened — an occupied slot is never displaced. The one state that
    /// is already correct without arming is `Claimed` for this exact key, which is what a
    /// direct transaction's rollback re-installs; there the owner still holds the incarnation
    /// and settles it with `restore`.
    pub(crate) fn waiter_ownership_reconcile_arrival(
        &mut self,
        idx: usize,
        record: super::defs::EndpointWaiterRecord,
    ) -> WaiterOwnershipTransition {
        let Some(key) = self.waiter_ownership_key_for(idx, record) else {
            return WaiterOwnershipTransition::NoSlot;
        };
        match self.waiter_ownership_view(&key) {
            WaiterOwnershipView::EndpointIndexOutOfRange => WaiterOwnershipTransition::NoSlot,
            WaiterOwnershipView::Available => WaiterOwnershipTransition::AlreadyConsistent,
            WaiterOwnershipView::Claimed { owner } => {
                WaiterOwnershipTransition::HeldByOwner { owner }
            }
            WaiterOwnershipView::Consumed => WaiterOwnershipTransition::ArmRefused {
                holding: WaiterSlotKind::Consumed,
            },
            WaiterOwnershipView::Cancelled => WaiterOwnershipTransition::ArmRefused {
                holding: WaiterSlotKind::Cancelled,
            },
            WaiterOwnershipView::ForeignIncarnation { holding } => {
                WaiterOwnershipTransition::ArmRefused { holding }
            }
            WaiterOwnershipView::Vacant => match self.waiter_ownership_arm_current(key) {
                Ok(()) => WaiterOwnershipTransition::Synchronized,
                Err(ArmError::EndpointIndexOutOfRange { .. }) => WaiterOwnershipTransition::NoSlot,
                Err(ArmError::SlotOccupied { holding }) => {
                    WaiterOwnershipTransition::ArmRefused { holding }
                }
            },
        }
    }

    /// Stage 199D-WA3C2 — reclaim this endpoint index's ownership slot when it still names an
    /// incarnation of a SUPERSEDED endpoint generation. Returns what was reclaimed, if any.
    ///
    /// Called from the publication pre-flight, because that is where a recycled index is first
    /// asked to accept a new waiter. See [`WaiterOwnershipTable::retire_superseded_generation`]
    /// for why an exact key is neither available nor needed here.
    pub(crate) fn waiter_ownership_retire_superseded(
        &mut self,
        idx: usize,
    ) -> Option<WaiterSlotKind> {
        let current = *self.endpoint_generations.get(idx)?;
        self.waiter_ownership
            .retire_superseded_generation(idx, current)
    }

    /// **Pre-flight: may `arriving` take this endpoint's ownership slot?** Mutates NOTHING.
    ///
    /// This runs before the waiter table moves, and it exists because "the departing record"
    /// is not enough to answer the question. A direct transaction REMOVES the waiter as part
    /// of claiming it, so at publication time the waiter table can be empty while the ownership
    /// slot is still `Claimed` — there is no displaced record to consult, and a check driven
    /// only by the displaced record would install the waiter first and discover the conflict
    /// after. Asking the slot directly is what keeps the refusal a true no-op.
    ///
    /// Admissible when the slot is `Vacant`, when it already names `arriving` (the rollback
    /// case: an owner restoring the exact incarnation it still holds), or when it names
    /// `departing` in a state [`Self::waiter_ownership_displace`] is able to end.
    pub(crate) fn waiter_ownership_publication_admits(
        &self,
        idx: usize,
        departing: Option<super::defs::EndpointWaiterRecord>,
        arriving: super::defs::EndpointWaiterRecord,
    ) -> WaiterOwnershipTransition {
        let Some(arriving_key) = self.waiter_ownership_key_for(idx, arriving) else {
            return WaiterOwnershipTransition::NoSlot;
        };
        match self.waiter_ownership_view(&arriving_key) {
            WaiterOwnershipView::EndpointIndexOutOfRange => WaiterOwnershipTransition::NoSlot,
            // Free, or already armed for this incarnation — both are states the publication can
            // legitimately land in.
            WaiterOwnershipView::Vacant | WaiterOwnershipView::Available => {
                WaiterOwnershipTransition::Synchronized
            }
            WaiterOwnershipView::Claimed { owner } => {
                WaiterOwnershipTransition::HeldByOwner { owner }
            }
            // Terminal-and-unretired for THIS incarnation: fail closed. Re-publishing over a
            // spent incarnation would resurrect a key an owner already settled.
            WaiterOwnershipView::Consumed => WaiterOwnershipTransition::ArmRefused {
                holding: WaiterSlotKind::Consumed,
            },
            WaiterOwnershipView::Cancelled => WaiterOwnershipTransition::ArmRefused {
                holding: WaiterSlotKind::Cancelled,
            },
            // Someone else holds the slot. Admissible only if that someone is exactly the
            // record being displaced, in a state the displacement transition can end.
            WaiterOwnershipView::ForeignIncarnation { holding } => {
                let departing_key =
                    departing.and_then(|old| self.waiter_ownership_key_for(idx, old));
                match departing_key.map(|key| self.waiter_ownership_view(&key)) {
                    Some(WaiterOwnershipView::Available)
                    | Some(WaiterOwnershipView::Consumed)
                    | Some(WaiterOwnershipView::Cancelled) => {
                        WaiterOwnershipTransition::Synchronized
                    }
                    Some(WaiterOwnershipView::Claimed { owner }) => {
                        WaiterOwnershipTransition::HeldByOwner { owner }
                    }
                    _ => WaiterOwnershipTransition::ArmRefused { holding },
                }
            }
        }
    }

    /// The **displaced-receiver transition**: end a departing incarnation's ownership under a
    /// named owner, rather than dropping it.
    ///
    /// CLAIM([`WaiterOwner::Teardown`]) → CANCEL → RETIRE, in one rank-3 section. Claiming
    /// first is what makes the transition *exclusive*: if a direct transaction already owns the
    /// departing incarnation, the claim is refused with `AlreadyClaimed` and the whole
    /// displacement is reported as [`WaiterOwnershipTransition::HeldByOwner`] so the publisher
    /// refuses instead of racing it. CANCEL rather than CONSUME because a displaced receiver
    /// was abandoned, not delivered to.
    pub(crate) fn waiter_ownership_displace(
        &mut self,
        idx: usize,
        departing: super::defs::EndpointWaiterRecord,
    ) -> WaiterOwnershipTransition {
        let Some(key) = self.waiter_ownership_key_for(idx, departing) else {
            return WaiterOwnershipTransition::NoSlot;
        };
        let token = match self.waiter_ownership_claim(key, WaiterOwner::Teardown) {
            Ok(token) => token,
            Err(ClaimError::AlreadyClaimed { by }) => {
                return WaiterOwnershipTransition::HeldByOwner { owner: by };
            }
            Err(ClaimError::NoCurrentWaiter) => {
                // Nothing armed for this endpoint index: the departing record predates the
                // ownership wiring or was already retired. Nothing to end.
                return WaiterOwnershipTransition::AlreadyConsistent;
            }
            Err(ClaimError::NoSuchCurrentIncarnation { holding }) => {
                return WaiterOwnershipTransition::ForeignIncarnation { holding };
            }
            // Terminal-but-unretired for exactly this incarnation, or a fail-closed claim
            // counter: there is no live owner to displace, so retiring it directly is both
            // sufficient and exactly what `reconcile_departure` does.
            Err(ClaimError::Consumed)
            | Err(ClaimError::Cancelled)
            | Err(ClaimError::ClaimGenerationExhausted)
            | Err(ClaimError::EndpointIndexOutOfRange { .. }) => {
                return self.waiter_ownership_reconcile_departure(idx, departing);
            }
        };
        // CANCEL cannot fail for a token this section just minted: the slot is `Claimed` with
        // this exact key, owner and claim generation, and nothing between the two calls can
        // touch it — both run under the one rank-3 acquisition. Settling is still checked
        // rather than assumed, because an unsettled claim would wedge the endpoint index.
        if self.waiter_ownership_cancel(token).is_err() {
            return WaiterOwnershipTransition::HeldByOwner {
                owner: WaiterOwner::Teardown,
            };
        }
        self.waiter_ownership_reconcile_departure(idx, departing)
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

    /// Arm + claim in one step, for tests whose subject is not the arming.
    fn armed_claim(
        t: &mut WaiterOwnershipTable,
        k: WaiterKey,
        owner: WaiterOwner,
    ) -> WaiterClaimToken {
        t.arm_current(k)
            .unwrap_or_else(|e| panic!("arm {k:?}: {e:?}"));
        t.claim(k, owner)
            .unwrap_or_else(|e| panic!("claim {k:?}: {e:?}"))
    }

    /// The three ways a key can name a different incarnation of the same endpoint index.
    fn other_incarnations() -> [WaiterKey; 3] {
        [
            // the endpoint was destroyed and recreated at the same index
            WaiterKey {
                endpoint_generation: base().endpoint_generation + 1,
                ..base()
            },
            // the same numeric TID, under a new address space
            key(3, 7, 42, 2 ^ 0x55, 11),
            // the same task, unblocked and reblocked
            WaiterKey {
                wait_generation: base().wait_generation + 1,
                ..base()
            },
        ]
    }

    // ── B. Reverse chronology: a stale incarnation cannot move the slot backward ─────────────

    /// **The R2 defect, in the exact shape reported.** Under WA2A-R1 this delayed claim returned
    /// a fresh token, because "key differs from the terminal key" was read as "a newer
    /// incarnation is taking over". Chronology is not derivable from a key, so `claim` no longer
    /// installs one.
    #[test]
    fn a_delayed_claim_for_a_consumed_and_retired_incarnation_is_refused() {
        let mut t = table();
        let a = base();
        let b = WaiterKey {
            wait_generation: a.wait_generation + 1,
            ..a
        };

        let ta = armed_claim(&mut t, a, WaiterOwner::DirectRequest);
        assert_eq!(t.consume(ta), Ok(()));
        assert_eq!(t.retire_current(a), Ok(()));

        let tb = armed_claim(&mut t, b, WaiterOwner::DirectRequest);
        assert_eq!(t.consume(tb), Ok(()));

        assert_eq!(
            t.claim(a, WaiterOwner::DirectRequest),
            Err(ClaimError::NoSuchCurrentIncarnation {
                holding: WaiterSlotKind::Consumed
            }),
            "A is older than B and long finished; a delayed claim must never mint a token"
        );
        assert_eq!(
            t.view(&b),
            WaiterOwnershipView::Consumed,
            "and B's terminal record is untouched"
        );
        assert_eq!(
            t.view(&a),
            WaiterOwnershipView::ForeignIncarnation {
                holding: WaiterSlotKind::Consumed
            },
            "A is not vacant, not available, and not claimable"
        );
    }

    #[test]
    fn a_delayed_claim_for_a_cancelled_and_retired_incarnation_is_refused() {
        let mut t = table();
        let a = base();
        let b = WaiterKey {
            wait_generation: a.wait_generation + 1,
            ..a
        };

        let ta = armed_claim(&mut t, a, WaiterOwner::Teardown);
        assert_eq!(t.cancel(ta), Ok(()));
        assert_eq!(t.retire_current(a), Ok(()));
        t.arm_current(b).expect("B becomes the current incarnation");

        assert_eq!(
            t.claim(a, WaiterOwner::DirectRequest),
            Err(ClaimError::NoSuchCurrentIncarnation {
                holding: WaiterSlotKind::Available
            })
        );
        assert_eq!(t.view(&b), WaiterOwnershipView::Available, "B is intact");
        // …and B can still be claimed normally.
        assert!(t.claim(b, WaiterOwner::DirectRequest).is_ok());
    }

    /// Every identity dimension refuses a delayed request once a newer incarnation is armed.
    #[test]
    fn an_older_incarnation_is_refused_in_every_identity_dimension() {
        for newer in other_incarnations() {
            let mut t = table();
            let old = base();
            let told = armed_claim(&mut t, old, WaiterOwner::DirectRequest);
            assert_eq!(t.consume(told), Ok(()));
            assert_eq!(t.retire_current(old), Ok(()));
            t.arm_current(newer).expect("the newer incarnation arms");

            assert_eq!(
                t.claim(old, WaiterOwner::LegacyDelivery),
                Err(ClaimError::NoSuchCurrentIncarnation {
                    holding: WaiterSlotKind::Available
                }),
                "an older incarnation must not claim over {newer:?}"
            );
            assert_eq!(
                t.view(&old),
                WaiterOwnershipView::ForeignIncarnation {
                    holding: WaiterSlotKind::Available
                }
            );
            assert_eq!(t.view(&newer), WaiterOwnershipView::Available);
        }
    }

    /// An old arm or retire request cannot erase or displace the newer incarnation either.
    #[test]
    fn a_stale_arm_or_retire_can_neither_erase_nor_replace_the_current_incarnation() {
        for newer in other_incarnations() {
            let mut t = table();
            let old = base();
            t.arm_current(newer).expect("newer arms first");

            assert_eq!(
                t.arm_current(old),
                Err(ArmError::SlotOccupied {
                    holding: WaiterSlotKind::Available
                }),
                "a late arm for {old:?} must not displace {newer:?}"
            );
            assert_eq!(
                t.retire_current(old),
                Err(RetireError::NoSuchCurrentIncarnation {
                    holding: WaiterSlotKind::Available
                }),
                "a late retire for {old:?} must not clear {newer:?}"
            );
            assert_eq!(
                t.view(&newer),
                WaiterOwnershipView::Available,
                "the current incarnation is untouched by both"
            );

            // Same, once the newer incarnation is claimed and then terminal.
            let tn = t.claim(newer, WaiterOwner::DirectReply).expect("claim");
            assert_eq!(
                t.retire_current(old),
                Err(RetireError::NoSuchCurrentIncarnation {
                    holding: WaiterSlotKind::Claimed
                })
            );
            assert_eq!(t.consume(tn), Ok(()));
            assert_eq!(
                t.retire_current(old),
                Err(RetireError::NoSuchCurrentIncarnation {
                    holding: WaiterSlotKind::Consumed
                })
            );
            assert_eq!(t.view(&newer), WaiterOwnershipView::Consumed);
        }
    }

    /// A live claim is inviolable: it cannot be armed over, retired, or claimed by anyone else.
    #[test]
    fn a_live_claim_can_be_neither_armed_over_nor_retired_nor_evicted() {
        let mut t = table();
        let live = armed_claim(&mut t, base(), WaiterOwner::DirectRequest);

        for other in other_incarnations() {
            assert_eq!(
                t.arm_current(other),
                Err(ArmError::SlotOccupied {
                    holding: WaiterSlotKind::Claimed
                }),
                "{other:?} must not arm over a live claim"
            );
            assert_eq!(
                t.claim(other, WaiterOwner::Teardown),
                Err(ClaimError::NoSuchCurrentIncarnation {
                    holding: WaiterSlotKind::Claimed
                })
            );
            assert_eq!(
                t.retire_current(other),
                Err(RetireError::NoSuchCurrentIncarnation {
                    holding: WaiterSlotKind::Claimed
                })
            );
        }
        // Not even the exact incarnation may be retired while claimed…
        assert_eq!(
            t.retire_current(base()),
            Err(RetireError::LiveClaim {
                by: WaiterOwner::DirectRequest
            }),
            "the owner must settle before the slot is released"
        );
        // …nor re-armed, nor claimed a second time.
        assert_eq!(
            t.arm_current(base()),
            Err(ArmError::SlotOccupied {
                holding: WaiterSlotKind::Claimed
            })
        );
        assert_eq!(
            t.claim(base(), WaiterOwner::OrdinaryTimeout),
            Err(ClaimError::AlreadyClaimed {
                by: WaiterOwner::DirectRequest
            })
        );
        assert_eq!(
            t.view(&base()),
            WaiterOwnershipView::Claimed {
                owner: WaiterOwner::DirectRequest
            },
            "and every refusal mutated nothing"
        );
        assert_eq!(t.consume(live), Ok(()), "the real owner still settles");
        assert_eq!(t.retire_current(base()), Ok(()), "and then it retires");
    }

    // ── A. The lifecycle itself ─────────────────────────────────────────────────────────────

    #[test]
    fn claim_never_installs_a_key_so_an_unarmed_slot_has_nothing_to_take() {
        let mut t = table();
        assert_eq!(
            t.claim(base(), WaiterOwner::DirectRequest),
            Err(ClaimError::NoCurrentWaiter),
            "claim must not arm the slot itself"
        );
        assert_eq!(t.view(&base()), WaiterOwnershipView::Vacant);
        assert_eq!(t.occupied_count(), 0, "and nothing was recorded");
    }

    #[test]
    fn arming_never_displaces_whatever_the_slot_already_holds() {
        for (settle, holding) in [
            (None, WaiterSlotKind::Available),
            (Some(true), WaiterSlotKind::Consumed),
            (Some(false), WaiterSlotKind::Cancelled),
        ] {
            let mut t = table();
            t.arm_current(base()).expect("arm");
            if let Some(consume) = settle {
                let token = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
                if consume {
                    assert_eq!(t.consume(token), Ok(()));
                } else {
                    assert_eq!(t.cancel(token), Ok(()));
                }
            }
            // Neither the same incarnation nor a different one may arm over it.
            for k in [base(), other_incarnations()[0]] {
                assert_eq!(
                    t.arm_current(k),
                    Err(ArmError::SlotOccupied { holding }),
                    "arming over {holding:?} must be refused for {k:?}"
                );
            }
        }
    }

    #[test]
    fn restore_returns_the_exact_incarnation_to_available_not_to_vacant() {
        let mut t = table();
        let token = armed_claim(&mut t, base(), WaiterOwner::DirectRequest);
        assert_eq!(t.restore(token), Ok(()));
        assert_eq!(
            t.slots[base().endpoint_index],
            WaiterOwnershipSlot::Available { key: base() },
            "the waiter is still published, so the slot keeps naming it"
        );
        assert_eq!(t.view(&base()), WaiterOwnershipView::Available);
        assert_eq!(t.claimed_count(), 0);
        assert_eq!(t.occupied_count(), 1, "restore does NOT unarm");
        // Crucially, an older delayed request still cannot take it over…
        for other in other_incarnations() {
            assert_eq!(
                t.claim(other, WaiterOwner::Teardown),
                Err(ClaimError::NoSuchCurrentIncarnation {
                    holding: WaiterSlotKind::Available
                })
            );
            assert_eq!(
                t.arm_current(other),
                Err(ArmError::SlotOccupied {
                    holding: WaiterSlotKind::Available
                })
            );
        }
        // …and the same incarnation is claimable again, by anyone.
        let second = t
            .claim(base(), WaiterOwner::LegacyDelivery)
            .expect("reclaim");
        assert_ne!(second.claim_generation(), token.claim_generation());
    }

    #[test]
    fn retire_releases_only_a_settled_or_unclaimed_exact_incarnation() {
        for settle in [None, Some(true), Some(false)] {
            let mut t = table();
            t.arm_current(base()).expect("arm");
            if let Some(consume) = settle {
                let token = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
                if consume {
                    assert_eq!(t.consume(token), Ok(()));
                } else {
                    assert_eq!(t.cancel(token), Ok(()));
                }
            }
            assert_eq!(t.retire_current(base()), Ok(()));
            assert_eq!(t.view(&base()), WaiterOwnershipView::Vacant);
            assert_eq!(t.occupied_count(), 0);
            // A second retire has nothing to release.
            assert_eq!(t.retire_current(base()), Err(RetireError::NotArmed));
        }
    }

    #[test]
    fn an_out_of_range_endpoint_index_fails_closed_on_every_operation() {
        let mut t = table();
        let bad = key(ENDPOINT_WAITER_SLOTS, 1, 1, 1, 1);
        let oor = ENDPOINT_WAITER_SLOTS;
        assert_eq!(
            t.arm_current(bad),
            Err(ArmError::EndpointIndexOutOfRange { index: oor })
        );
        assert_eq!(
            t.claim(bad, WaiterOwner::DirectRequest),
            Err(ClaimError::EndpointIndexOutOfRange { index: oor })
        );
        assert_eq!(
            t.retire_current(bad),
            Err(RetireError::EndpointIndexOutOfRange { index: oor })
        );
        assert_eq!(
            t.view(&bad),
            WaiterOwnershipView::EndpointIndexOutOfRange,
            "an out-of-range index must never read as Vacant — that would imply it is claimable"
        );
        assert_eq!(t.occupied_count(), 0);
        // A token whose key is out of range cannot settle either.
        let live = armed_claim(&mut t, base(), WaiterOwner::DirectRequest);
        assert_eq!(
            t.consume(live.forged_with_key(bad)),
            Err(SettleError::EndpointIndexOutOfRange { index: oor })
        );
    }

    // ── C. The view is truthful ─────────────────────────────────────────────────────────────

    /// **The exact view contract**, in three parts:
    ///
    /// 1. every non-`Available` view *necessarily* rejects a claim;
    /// 2. an ordinary (non-exhausted) `Available` view admits one;
    /// 3. an **exhausted** `Available` view stays `Available` and rejects with
    ///    `ClaimGenerationExhausted`, mutating nothing.
    ///
    /// Part 3 is why the contract is "the only state structurally eligible for a claim" rather
    /// than "a claim would succeed": the earlier wording was not literally true.
    #[test]
    fn available_is_the_only_claim_eligible_view_but_is_not_a_promise_of_success() {
        let bad = key(ENDPOINT_WAITER_SLOTS, 1, 1, 1, 1);
        let foreign = other_incarnations()[0];

        // Vacant → NoCurrentWaiter (not claimable, but nothing holds it).
        let mut t = table();
        assert_eq!(t.view(&base()), WaiterOwnershipView::Vacant);
        assert_eq!(
            t.claim(base(), WaiterOwner::DirectRequest),
            Err(ClaimError::NoCurrentWaiter)
        );

        // Available → the only claim-eligible state (part 2: an ordinary one admits a claim).
        t.arm_current(base()).expect("arm");
        assert_eq!(t.view(&base()), WaiterOwnershipView::Available);
        let token = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");

        // Claimed, viewed by its own key and by a foreign one.
        assert_eq!(
            t.view(&base()),
            WaiterOwnershipView::Claimed {
                owner: WaiterOwner::DirectRequest
            }
        );
        assert_eq!(
            t.view(&foreign),
            WaiterOwnershipView::ForeignIncarnation {
                holding: WaiterSlotKind::Claimed
            },
            "a foreign key over a live claim must NEVER read Vacant"
        );
        assert!(t.claim(foreign, WaiterOwner::Teardown).is_err());

        // Terminal.
        assert_eq!(t.consume(token), Ok(()));
        assert_eq!(t.view(&base()), WaiterOwnershipView::Consumed);
        assert_eq!(
            t.view(&foreign),
            WaiterOwnershipView::ForeignIncarnation {
                holding: WaiterSlotKind::Consumed
            }
        );

        // Out of range.
        assert_eq!(t.view(&bad), WaiterOwnershipView::EndpointIndexOutOfRange);

        // Part 1, exhaustively: whenever the view is not `Available`, a claim NECESSARILY fails.
        for (label, view, k) in [
            ("vacant", t.view(&key(9, 1, 1, 1, 1)), key(9, 1, 1, 1, 1)),
            ("consumed-exact", t.view(&base()), base()),
            ("foreign", t.view(&foreign), foreign),
            ("out-of-range", t.view(&bad), bad),
        ] {
            assert_ne!(view, WaiterOwnershipView::Available, "{label}");
            assert!(
                t.claim(k, WaiterOwner::DirectRequest).is_err(),
                "{label}: a non-Available view is not claim-eligible and claim must agree"
            );
        }

        // Part 3: `Available` is eligibility, NOT a promise. With the generation counter
        // saturated the very same view rejects — and leaves the slot exactly as it was.
        let mut ex = table();
        ex.arm_current(base()).expect("arm");
        ex.next_claim_generation = u64::MAX;
        assert_eq!(
            ex.view(&base()),
            WaiterOwnershipView::Available,
            "the view reports eligibility, and it does not leak counter state"
        );
        assert_eq!(
            ex.claim(base(), WaiterOwner::DirectRequest),
            Err(ClaimError::ClaimGenerationExhausted),
            "an eligible slot can still fail closed"
        );
        assert_eq!(
            ex.view(&base()),
            WaiterOwnershipView::Available,
            "and the refusal mutated nothing — still armed, still unclaimed"
        );
        assert_eq!(ex.claimed_count(), 0);
        assert_eq!(ex.occupied_count(), 1);
    }

    #[test]
    fn the_view_carries_no_claim_generation_anywhere() {
        let mut t = table();
        let first = armed_claim(&mut t, base(), WaiterOwner::DirectRequest);
        assert_eq!(t.restore(first), Ok(()));
        let second = t
            .claim(base(), WaiterOwner::DirectRequest)
            .expect("reclaim");
        assert_ne!(first.claim_generation(), second.claim_generation());
        // The view is identical across two different live generations, so it leaks nothing an
        // observer could use to reconstruct a token.
        assert_eq!(
            t.view(&base()),
            WaiterOwnershipView::Claimed {
                owner: WaiterOwner::DirectRequest
            }
        );
    }

    // ── Bounded and leak-free ───────────────────────────────────────────────────────────────

    #[test]
    fn the_capacity_is_exactly_the_endpoint_waiter_bound() {
        assert_eq!(WAITER_OWNERSHIP_SLOTS, ENDPOINT_WAITER_SLOTS);
        assert_eq!(table().slots.len(), ENDPOINT_WAITER_SLOTS);
    }

    #[test]
    fn every_endpoint_slot_can_be_armed_and_claimed_simultaneously() {
        let mut t = table();
        let mut tokens = alloc::vec::Vec::new();
        for i in 0..ENDPOINT_WAITER_SLOTS {
            tokens.push(armed_claim(
                &mut t,
                key(i, 1, i as u64, 1, 1),
                WaiterOwner::DirectRequest,
            ));
        }
        assert_eq!(t.claimed_count(), ENDPOINT_WAITER_SLOTS);
        assert_eq!(t.occupied_count(), ENDPOINT_WAITER_SLOTS);
        // Fully unwinding returns every slot to Vacant.
        for token in tokens {
            assert_eq!(t.restore(token), Ok(()));
        }
        assert_eq!(t.claimed_count(), 0);
        for i in 0..ENDPOINT_WAITER_SLOTS {
            assert_eq!(t.retire_current(key(i, 1, i as u64, 1, 1)), Ok(()));
        }
        assert_eq!(t.occupied_count(), 0);
    }

    /// Explicit retirement is what keeps the endpoint-indexed table leak-free: 10 001 complete
    /// cycles over changing wait generations leave the slot exactly as it started.
    #[test]
    fn ten_thousand_and_one_complete_arm_claim_consume_retire_cycles_leak_nothing() {
        let mut t = table();
        for wait_generation in 0..10_001u64 {
            let k = WaiterKey {
                wait_generation,
                ..base()
            };
            t.arm_current(k)
                .unwrap_or_else(|e| panic!("arm {wait_generation}: {e:?}"));
            let token = t
                .claim(k, WaiterOwner::DirectRequest)
                .unwrap_or_else(|e| panic!("claim {wait_generation}: {e:?}"));
            assert_eq!(t.consume(token), Ok(()));
            assert_eq!(t.retire_current(k), Ok(()));
            assert_eq!(t.occupied_count(), 0);
        }
        assert_eq!(t.claimed_count(), 0);
        assert_eq!(t.occupied_count(), 0);
    }

    #[test]
    fn ten_thousand_and_one_arm_claim_cancel_retire_cycles_leak_nothing() {
        let mut t = table();
        for wait_generation in 0..10_001u64 {
            let k = WaiterKey {
                wait_generation,
                ..base()
            };
            t.arm_current(k).expect("arm");
            let token = t.claim(k, WaiterOwner::Teardown).expect("claim");
            assert_eq!(t.cancel(token), Ok(()));
            assert_eq!(t.view(&k), WaiterOwnershipView::Cancelled);
            assert_eq!(t.retire_current(k), Ok(()));
        }
        assert_eq!(t.occupied_count(), 0);
    }

    #[test]
    fn ten_thousand_and_one_arm_claim_restore_retire_cycles_leak_nothing() {
        let mut t = table();
        for wait_generation in 0..10_001u64 {
            let k = WaiterKey {
                wait_generation,
                ..base()
            };
            t.arm_current(k).expect("arm");
            let token = t.claim(k, WaiterOwner::DirectReply).expect("claim");
            assert_eq!(t.restore(token), Ok(()));
            assert_eq!(t.claimed_count(), 0);
            assert_eq!(t.retire_current(k), Ok(()));
        }
        assert_eq!(t.occupied_count(), 0);
    }

    // ── One winner ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn exactly_one_of_two_competing_owners_wins() {
        let mut t = table();
        let token = armed_claim(&mut t, base(), WaiterOwner::DirectRequest);
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
        let first = armed_claim(&mut t, base(), WaiterOwner::DirectReply);
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
        let real = armed_claim(&mut t, base(), WaiterOwner::DirectRequest);
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

    // ── Token staleness ─────────────────────────────────────────────────────────────────────

    #[test]
    fn a_token_for_a_superseded_incarnation_is_no_such_claim() {
        for newer in other_incarnations() {
            let mut t = table();
            let old = armed_claim(&mut t, base(), WaiterOwner::DirectRequest);
            assert_eq!(t.restore(old), Ok(()));
            assert_eq!(t.retire_current(base()), Ok(()));
            let new = armed_claim(&mut t, newer, WaiterOwner::DirectRequest);
            for (what, r) in [
                ("consume", t.consume(old)),
                ("restore", t.restore(old)),
                ("cancel", t.cancel(old)),
            ] {
                assert_eq!(
                    r,
                    Err(SettleError::NoSuchClaim),
                    "{what} with a token for a superseded incarnation must not touch {newer:?}"
                );
            }
            assert_eq!(
                t.view(&newer),
                WaiterOwnershipView::Claimed {
                    owner: WaiterOwner::DirectRequest
                },
                "the current incarnation is untouched"
            );
            assert_eq!(t.consume(new), Ok(()));
        }
    }

    /// **The claim generation alone must reject a stale token.** Restored and re-claimed by the
    /// *same* owner for the *same* incarnation, so neither the owner comparison nor the key
    /// comparison can mask it.
    #[test]
    fn a_stale_token_is_rejected_even_when_the_same_owner_reclaims() {
        let mut t = table();
        let first = armed_claim(&mut t, base(), WaiterOwner::DirectRequest);
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

    #[test]
    fn a_settled_incarnation_cannot_be_re_armed_or_re_claimed_before_retirement() {
        for (settle_is_consume, expect_claim, expect_view, holding) in [
            (
                true,
                ClaimError::Consumed,
                WaiterOwnershipView::Consumed,
                WaiterSlotKind::Consumed,
            ),
            (
                false,
                ClaimError::Cancelled,
                WaiterOwnershipView::Cancelled,
                WaiterSlotKind::Cancelled,
            ),
        ] {
            let mut t = table();
            let token = armed_claim(&mut t, base(), WaiterOwner::DirectRequest);
            if settle_is_consume {
                assert_eq!(t.consume(token), Ok(()));
            } else {
                assert_eq!(t.cancel(token), Ok(()));
            }
            assert_eq!(t.view(&base()), expect_view);
            for owner in [
                WaiterOwner::DirectRequest,
                WaiterOwner::OrdinaryTimeout,
                WaiterOwner::Notification,
                WaiterOwner::Teardown,
            ] {
                assert_eq!(t.claim(base(), owner), Err(expect_claim));
            }
            assert_eq!(
                t.arm_current(base()),
                Err(ArmError::SlotOccupied { holding })
            );
            assert_eq!(
                t.restore(token),
                Err(SettleError::NotClaimed { state: holding })
            );
            assert_eq!(
                t.view(&base()),
                expect_view,
                "a refused settle mutates nothing"
            );
        }
    }

    // ── Generation safety ───────────────────────────────────────────────────────────────────

    #[test]
    fn claim_generations_start_at_one_and_never_repeat() {
        let mut t = table();
        let first = armed_claim(&mut t, base(), WaiterOwner::DirectRequest);
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
        t.arm_current(base()).expect("arm");
        t.next_claim_generation = u64::MAX;
        assert_eq!(
            t.claim(base(), WaiterOwner::DirectRequest),
            Err(ClaimError::ClaimGenerationExhausted)
        );
        assert_eq!(
            t.slots[base().endpoint_index],
            WaiterOwnershipSlot::Available { key: base() },
            "the armed incarnation is untouched"
        );
        assert_eq!(t.claimed_count(), 0);
        assert_eq!(
            t.next_claim_generation,
            u64::MAX,
            "the counter never wraps past its last usable value"
        );
        assert_eq!(
            t.claim(base(), WaiterOwner::Teardown),
            Err(ClaimError::ClaimGenerationExhausted),
            "and it stays closed"
        );
    }

    /// The last usable generation is still issued, and only then does the table close.
    #[test]
    fn the_final_usable_generation_is_issued_before_exhaustion() {
        let mut t = table();
        t.arm_current(base()).expect("arm");
        t.next_claim_generation = u64::MAX - 1;
        let token = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        assert_eq!(token.claim_generation(), u64::MAX - 1);
        assert_eq!(t.restore(token), Ok(()));
        assert_eq!(
            t.claim(base(), WaiterOwner::DirectRequest),
            Err(ClaimError::ClaimGenerationExhausted)
        );
    }

    // ── The IpcSubsystem surface is the only usable one, and it works ───────────────────────

    /// Drives the whole lifecycle through the **public** surface — `&mut IpcSubsystem` handed out
    /// by the ipc rank-3 seam — rather than the module-private table methods. This is the exact
    /// shape a future owner will use: arm and claim under the guard, carry the `Copy` token out,
    /// settle and retire under a later guard.
    #[test]
    fn the_ipc_subsystem_surface_drives_a_full_incarnation_lifecycle() {
        let mut state = crate::kernel::boot::Bootstrap::init().expect("init");
        let k = base();

        // Arm + claim under one rank-3 acquisition…
        let token = state
            .with_ipc_state_mut(|ipc| {
                assert_eq!(ipc.waiter_ownership_view(&k), WaiterOwnershipView::Vacant);
                assert_eq!(
                    ipc.waiter_ownership_claim(k, WaiterOwner::DirectRequest),
                    Err(ClaimError::NoCurrentWaiter),
                    "nothing is armed yet"
                );
                ipc.waiter_ownership_arm_current(k).expect("arm");
                assert_eq!(
                    ipc.waiter_ownership_view(&k),
                    WaiterOwnershipView::Available
                );
                ipc.waiter_ownership_claim(k, WaiterOwner::DirectRequest)
            })
            .expect("claim");
        // …the token survives the guard being dropped, and reports exactly the three facts a
        // future owner needs — never the claim generation.
        assert_eq!(token.owner(), WaiterOwner::DirectRequest);
        assert_eq!(token.endpoint_index(), k.endpoint_index);
        assert_eq!(token.waiter(), k.waiter);

        // A competitor, and a stale incarnation, both lose under a *later* acquisition.
        state.with_ipc_state_mut(|ipc| {
            assert_eq!(
                ipc.waiter_ownership_claim(k, WaiterOwner::OrdinaryTimeout),
                Err(ClaimError::AlreadyClaimed {
                    by: WaiterOwner::DirectRequest
                })
            );
            let stale = other_incarnations()[2];
            assert_eq!(
                ipc.waiter_ownership_claim(stale, WaiterOwner::Notification),
                Err(ClaimError::NoSuchCurrentIncarnation {
                    holding: WaiterSlotKind::Claimed
                })
            );
            assert_eq!(
                ipc.waiter_ownership_retire_current(k),
                Err(RetireError::LiveClaim {
                    by: WaiterOwner::DirectRequest
                })
            );
            assert_eq!(ipc.waiter_ownership_claimed_count(), 1);
            assert_eq!(ipc.waiter_ownership_occupied_count(), 1);
        });

        // Restore, re-claim, consume, retire, then the next incarnation — all through the seam.
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
            assert_eq!(ipc.waiter_ownership_retire_current(k), Ok(()));
            assert_eq!(ipc.waiter_ownership_occupied_count(), 0);

            let next = WaiterKey {
                wait_generation: k.wait_generation + 1,
                ..k
            };
            ipc.waiter_ownership_arm_current(next).expect("arm next");
            let t3 = ipc
                .waiter_ownership_claim(next, WaiterOwner::Teardown)
                .expect("claim next");
            // The retired predecessor cannot come back.
            assert_eq!(
                ipc.waiter_ownership_claim(k, WaiterOwner::DirectRequest),
                Err(ClaimError::NoSuchCurrentIncarnation {
                    holding: WaiterSlotKind::Claimed
                })
            );
            assert_eq!(ipc.waiter_ownership_cancel(t3), Ok(()));
            assert_eq!(ipc.waiter_ownership_retire_current(next), Ok(()));
            assert_eq!(ipc.waiter_ownership_occupied_count(), 0);
        });
    }

    /// A freshly booted kernel arms nothing: the primitive is helper-only, so no live path
    /// touches it.
    #[test]
    fn a_booted_kernel_holds_no_armed_incarnations() {
        let mut state = crate::kernel::boot::Bootstrap::init().expect("init");
        state.with_ipc_state_mut(|ipc| {
            assert_eq!(ipc.waiter_ownership_claimed_count(), 0);
            assert_eq!(ipc.waiter_ownership_occupied_count(), 0);
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
