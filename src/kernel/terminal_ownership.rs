// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 200A — Reply / Timeout / Peer-Death terminal ownership state machine.
//!
//! This module defines ONE authoritative, architecture-neutral state machine
//! governing the outcome of a caller that is blocked on its reply endpoint after
//! an `IpcCall` (NR6). YARM keeps its call semantics: NR6 sends the request and
//! returns; the caller then explicitly recv-v2 blocks on its reply endpoint.
//! Timeout and server-death completion therefore operate on that blocked reply
//! receive and its associated reply record — NOT on a long-running NR6 syscall
//! frame.
//!
//! ## Single persistent authority (`authority_stores = 1`)
//!
//! Every terminal operation — reply, timeout, peer death, caller exit and
//! endpoint destruction — claims through the SAME [`TerminalCell`]. There is NO
//! second independently authoritative timeout or peer-death table. A reply
//! reservation is expressed as ONE terminal claimant ([`TerminalClaimant::Reply`])
//! rather than a competing authority system. The accepted NR7 reservation
//! (`Available → ReplyReserved → Consumed`) maps onto:
//!
//! ```text
//!   Open(Live/Available) → Reserved(Reply) → Completed(Reply)   [NR7 success]
//! ```
//!
//! The conceptual lifecycle across all claimants is a fan-in — exactly one
//! claimant may reserve, and only that owner may complete or release:
//!
//! ```text
//!                 ┌──► Reserved(Reply)        ─┐
//!                 ├──► Reserved(Timeout)       │
//!   Open ─────────┼──► Reserved(PeerDeath)     ├──► Completed   (terminal, one-shot)
//!  (Live/         ├──► Reserved(CallerExit)    │
//!   Available)    └──► Reserved(EndpointGone) ─┘
//!                        │
//!                        └── release_if_retryable ──► Open   (same generation)
//! ```
//!
//! ## Generation-bearing identity
//!
//! Every terminal operation is tied to a full [`TerminalIdentity`]: the reply
//! record index + generation, the caller `{tid, asid}`, the replier `{tid, asid}`,
//! the reply endpoint index + generation, the blocked-receive generation, and an
//! optional deadline-token generation. A numeric TID alone NEVER authorizes a
//! cancellation, wake or cleanup: a restarted caller or server that reuses the
//! same numeric TID carries a different ASID (and/or generation), so the exact
//! `==` identity comparison at claim time refuses it.
//!
//! ## Memory ordering (see the Stage 200A memory-ordering audit)
//!
//! * `arm` writes the immutable identity first, then publishes the `Open` state
//!   with a single `Release` store (LAST). A claimant `Acquire`-loads the state
//!   first, so observing `Open` means the complete identity is visible.
//! * The claim is a single `compare_exchange(OPEN, RESERVED, AcqRel, Acquire)`:
//!   exactly one claimant's CAS succeeds; the loser mutates nothing.
//! * `commit`/`release` are `compare_exchange(RESERVED, …, AcqRel, Acquire)` from
//!   the owner's exact `{claimant, epoch}` — only the owner transitions.
//! * `Completed` is terminal: there is no transition out of it, so a completed
//!   record can never be reopened, and a stale claimant (wrong epoch/generation)
//!   cannot mutate it.
//!
//! An internal monotonic `epoch` (bumped on every `arm`) is packed into the state
//! word as an ABA nonce: a stale owner token from a previous arming can never
//! `commit`/`release` a re-armed cell, even if the numeric claimant tag repeats.
//! This is distinct from the record generation carried in [`TerminalIdentity`]
//! (the externally meaningful discriminator).

use crate::kernel::ipc::ThreadId;
use crate::kernel::vm::Asid;
use core::sync::atomic::{AtomicU64, Ordering};

/// Which terminal operation owns (or is claiming) a blocked-reply record's
/// outcome. Only ONE claimant can win per record; the tag is packed into the
/// [`TerminalCell`] state word so the winning claim is atomic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TerminalClaimant {
    /// The bound replier delivering the reply (the accepted NR7 path).
    Reply = 1,
    /// A reply-receive deadline firing.
    Timeout = 2,
    /// The bound replier (server) dying while the caller is blocked.
    PeerDeath = 3,
    /// The blocked caller itself exiting.
    CallerExit = 4,
    /// The reply endpoint being destroyed.
    EndpointGone = 5,
}

impl TerminalClaimant {
    #[inline]
    const fn tag(self) -> u64 {
        self as u64
    }

    const fn from_tag(tag: u64) -> Option<Self> {
        match tag {
            1 => Some(Self::Reply),
            2 => Some(Self::Timeout),
            3 => Some(Self::PeerDeath),
            4 => Some(Self::CallerExit),
            5 => Some(Self::EndpointGone),
            _ => None,
        }
    }

    /// Recover the claimant from its numeric repr (e.g. a `kind as usize` tag).
    /// `None` for a tag that names no claimant (including `0`/open).
    pub const fn from_tag_usize(tag: usize) -> Option<Self> {
        Self::from_tag(tag as u64)
    }
}

/// Generation-bearing identity of a blocked-reply terminal record. Every field is
/// captured when the record is armed and is IMMUTABLE for that arming; a terminal
/// operation must present an EXACT `==` match to claim. Numeric TID alone is never
/// sufficient — the ASIDs and generations distinguish incarnations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalIdentity {
    /// The single persistent reply-record slot (index + generation) this terminal
    /// authority governs.
    pub reply_record_index: usize,
    pub reply_record_generation: u64,
    /// The blocked caller (requester) incarnation.
    pub caller_tid: ThreadId,
    pub caller_asid: Asid,
    /// The bound replier (responder) incarnation.
    pub replier_tid: ThreadId,
    pub replier_asid: Asid,
    /// The reply endpoint the caller is blocked on (index + generation).
    pub reply_endpoint_index: usize,
    pub reply_endpoint_generation: u64,
    /// The blocked recv-v2 generation the wake must match.
    pub blocked_recv_generation: u64,
    /// Optional deadline token generation (armed by Stage 200B); `None` when no
    /// deadline is registered for this record.
    pub deadline_token_generation: Option<u64>,
}

impl TerminalIdentity {
    /// The zeroed identity a vacant cell carries. No live claimant can ever
    /// present this (a real record always has a non-zero generation), so a vacant
    /// cell rejects every claim.
    pub const ZERO: Self = Self {
        reply_record_index: usize::MAX,
        reply_record_generation: 0,
        caller_tid: ThreadId(0),
        caller_asid: Asid(0),
        replier_tid: ThreadId(0),
        replier_asid: Asid(0),
        reply_endpoint_index: usize::MAX,
        reply_endpoint_generation: 0,
        blocked_recv_generation: 0,
        deadline_token_generation: None,
    };
}

// ── State-word packing ─────────────────────────────────────────────────────────
//
//   bit layout:  [ epoch (59) | claimant (3) | phase (2) ]
//
// * phase    — 0 = Open, 1 = Reserved, 2 = Completed.
// * claimant — 0 when Open; otherwise the winning [`TerminalClaimant`] tag (1..=5).
// * epoch    — a monotonic per-cell ABA nonce, bumped on every `arm`.
const PHASE_BITS: u64 = 2;
const PHASE_MASK: u64 = 0b11;
const CLAIMANT_BITS: u64 = 3;
const CLAIMANT_SHIFT: u64 = PHASE_BITS;
const CLAIMANT_MASK: u64 = 0b111 << CLAIMANT_SHIFT;
const EPOCH_SHIFT: u64 = PHASE_BITS + CLAIMANT_BITS;

const PHASE_OPEN: u64 = 0;
const PHASE_RESERVED: u64 = 1;
const PHASE_COMPLETED: u64 = 2;

#[inline]
const fn encode(epoch: u64, claimant_tag: u64, phase: u64) -> u64 {
    (epoch << EPOCH_SHIFT) | (claimant_tag << CLAIMANT_SHIFT) | phase
}

#[inline]
const fn phase_of(word: u64) -> u64 {
    word & PHASE_MASK
}

#[inline]
const fn claimant_tag_of(word: u64) -> u64 {
    (word & CLAIMANT_MASK) >> CLAIMANT_SHIFT
}

#[inline]
const fn epoch_of(word: u64) -> u64 {
    word >> EPOCH_SHIFT
}

/// The exclusive ownership token minted by a successful `try_claim_*`. It carries
/// the winning claimant and the exact arming `epoch`, and CANNOT be constructed
/// outside this module — so only the claim winner can `commit` or `release`. A
/// token from a previous arming (stale `epoch`) fails every transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalOwner {
    claimant: TerminalClaimant,
    epoch: u64,
}

impl TerminalOwner {
    #[inline]
    pub const fn claimant(&self) -> TerminalClaimant {
        self.claimant
    }
}

/// The single authoritative terminal-ownership cell for one blocked-reply record.
///
/// Exactly one lives per reply-record slot (co-located with the reply-cap store),
/// so reply/timeout/peer-death/caller-exit/endpoint-destruction all funnel through
/// the SAME authority. The identity is immutable per arming; only the packed state
/// word is atomic, transitioned by the bounded primitives below.
#[derive(Debug)]
pub struct TerminalCell {
    identity: TerminalIdentity,
    state: AtomicU64,
}

impl Default for TerminalCell {
    fn default() -> Self {
        Self::vacant()
    }
}

impl TerminalCell {
    /// A vacant cell (epoch 0, `Open`, zeroed identity). Const so a whole
    /// `[TerminalCell; N]` authority store can be initialized at boot. A vacant
    /// cell rejects every claim (its identity matches no live record).
    pub const fn vacant() -> Self {
        Self {
            identity: TerminalIdentity::ZERO,
            state: AtomicU64::new(encode(0, 0, PHASE_OPEN)),
        }
    }

    /// Arm (or re-arm) the cell for a fresh blocked-reply record. Bumps the
    /// internal `epoch` (ABA nonce), writes the immutable identity FIRST, then
    /// publishes `Open` with a single `Release` store LAST — so any claimant that
    /// `Acquire`-observes `Open` also observes the complete identity. Requires
    /// `&mut self` (exclusive: no claim can race an arming).
    pub fn arm(&mut self, identity: TerminalIdentity) {
        let prev = self.state.load(Ordering::Relaxed);
        let mut epoch = epoch_of(prev).wrapping_add(1);
        if epoch == 0 {
            epoch = 1;
        }
        self.identity = identity;
        // Release publication: identity write (above) is visible to any Acquire
        // reader that observes this Open state.
        self.state
            .store(encode(epoch, 0, PHASE_OPEN), Ordering::Release);
    }

    /// The generation-bearing identity this cell is armed for.
    #[inline]
    pub fn identity(&self) -> &TerminalIdentity {
        &self.identity
    }

    /// `true` iff no claimant currently owns the record (claimable).
    #[inline]
    pub fn is_open(&self) -> bool {
        phase_of(self.state.load(Ordering::Acquire)) == PHASE_OPEN
    }

    /// The claimant currently holding a `Reserved` claim, if any.
    #[inline]
    pub fn reserved_claimant(&self) -> Option<TerminalClaimant> {
        let w = self.state.load(Ordering::Acquire);
        if phase_of(w) == PHASE_RESERVED {
            TerminalClaimant::from_tag(claimant_tag_of(w))
        } else {
            None
        }
    }

    /// The claimant that WON the terminal outcome, once `Completed`. `None` while
    /// the record is still `Open` or merely `Reserved`.
    #[inline]
    pub fn committed_winner(&self) -> Option<TerminalClaimant> {
        let w = self.state.load(Ordering::Acquire);
        if phase_of(w) == PHASE_COMPLETED {
            TerminalClaimant::from_tag(claimant_tag_of(w))
        } else {
            None
        }
    }

    /// `true` once a terminal outcome has been committed (one-shot, terminal).
    #[inline]
    pub fn is_completed(&self) -> bool {
        phase_of(self.state.load(Ordering::Acquire)) == PHASE_COMPLETED
    }

    /// Core bounded claim. Atomically checks generation + identity and, on an exact
    /// match, transitions `Open → Reserved(claimant)` with a single AcqRel CAS.
    /// Returns the exclusive [`TerminalOwner`] to the sole winner; every stale or
    /// losing claimant gets `None` and mutates NOTHING.
    fn try_claim(
        &self,
        claimant: TerminalClaimant,
        expect: &TerminalIdentity,
    ) -> Option<TerminalOwner> {
        // Acquire-gate on the published state first.
        let w = self.state.load(Ordering::Acquire);
        if phase_of(w) != PHASE_OPEN {
            return None; // already reserved or completed — no second claimant
        }
        // Identity is published-before-Open (Release/Acquire), so this is a
        // complete, untorn read. Numeric TID alone never authorizes: the full
        // generation-bearing identity must match exactly.
        if self.identity != *expect {
            return None;
        }
        let epoch = epoch_of(w);
        let open = encode(epoch, 0, PHASE_OPEN);
        let reserved = encode(epoch, claimant.tag(), PHASE_RESERVED);
        match self
            .state
            .compare_exchange(open, reserved, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Some(TerminalOwner { claimant, epoch }),
            Err(_) => None, // lost the race — stale claimant mutates nothing
        }
    }

    /// Claim the terminal outcome for the bound-replier REPLY delivery (NR7).
    #[inline]
    pub fn try_claim_reply_terminal(&self, expect: &TerminalIdentity) -> Option<TerminalOwner> {
        self.try_claim(TerminalClaimant::Reply, expect)
    }

    /// Claim the terminal outcome for a reply-receive TIMEOUT.
    #[inline]
    pub fn try_claim_timeout_terminal(&self, expect: &TerminalIdentity) -> Option<TerminalOwner> {
        self.try_claim(TerminalClaimant::Timeout, expect)
    }

    /// Claim the terminal outcome for PEER (server) DEATH.
    #[inline]
    pub fn try_claim_peer_death_terminal(
        &self,
        expect: &TerminalIdentity,
    ) -> Option<TerminalOwner> {
        self.try_claim(TerminalClaimant::PeerDeath, expect)
    }

    /// Claim the terminal outcome for the blocked CALLER EXITING.
    #[inline]
    pub fn try_claim_caller_exit_terminal(
        &self,
        expect: &TerminalIdentity,
    ) -> Option<TerminalOwner> {
        self.try_claim(TerminalClaimant::CallerExit, expect)
    }

    /// Claim the terminal outcome for reply ENDPOINT DESTRUCTION.
    #[inline]
    pub fn try_claim_endpoint_gone_terminal(
        &self,
        expect: &TerminalIdentity,
    ) -> Option<TerminalOwner> {
        self.try_claim(TerminalClaimant::EndpointGone, expect)
    }

    /// Commit the owner's terminal outcome: `Reserved(owner) → Completed(owner)`
    /// with a single AcqRel CAS. Only the exact `{claimant, epoch}` owner succeeds;
    /// a second commit, a non-owner, or a stale-epoch token all fail closed. Once
    /// `Completed`, the record is terminal and can never be reopened.
    #[must_use]
    pub fn commit_terminal(&self, owner: &TerminalOwner) -> bool {
        let reserved = encode(owner.epoch, owner.claimant.tag(), PHASE_RESERVED);
        let completed = encode(owner.epoch, owner.claimant.tag(), PHASE_COMPLETED);
        self.state
            .compare_exchange(reserved, completed, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Release the owner's claim back to `Open` for a RETRYABLE rollback (e.g. a
    /// caller-destination copy fault while the exact caller is still valid):
    /// `Reserved(owner) → Open` at the SAME epoch, so a subsequent same-identity
    /// claimant may win. Only the exact owner succeeds; if any competing terminal
    /// claimant has already won (the state is no longer this owner's `Reserved`),
    /// the CAS fails and NO authority is restored.
    #[must_use]
    pub fn release_terminal_if_retryable(&self, owner: &TerminalOwner) -> bool {
        let reserved = encode(owner.epoch, owner.claimant.tag(), PHASE_RESERVED);
        let open = encode(owner.epoch, 0, PHASE_OPEN);
        self.state
            .compare_exchange(reserved, open, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(record_gen: u64) -> TerminalIdentity {
        TerminalIdentity {
            reply_record_index: 3,
            reply_record_generation: record_gen,
            caller_tid: ThreadId(1),
            caller_asid: Asid(11),
            replier_tid: ThreadId(2),
            replier_asid: Asid(22),
            reply_endpoint_index: 7,
            reply_endpoint_generation: 5,
            blocked_recv_generation: 9,
            deadline_token_generation: None,
        }
    }

    fn armed(record_gen: u64) -> TerminalCell {
        let mut c = TerminalCell::vacant();
        c.arm(ident(record_gen));
        c
    }

    #[test]
    fn vacant_cell_rejects_every_claim() {
        let c = TerminalCell::vacant();
        assert!(c.is_open());
        assert!(c.try_claim_reply_terminal(&ident(1)).is_none());
        assert!(c.try_claim_timeout_terminal(&ident(1)).is_none());
        assert!(c.committed_winner().is_none());
    }

    #[test]
    fn reply_reserve_then_commit_is_the_nr7_lifecycle() {
        let c = armed(1);
        let owner = c
            .try_claim_reply_terminal(&ident(1))
            .expect("reply claims Open");
        assert_eq!(c.reserved_claimant(), Some(TerminalClaimant::Reply));
        assert!(
            c.commit_terminal(&owner),
            "owner commits Reserved → Completed"
        );
        assert_eq!(c.committed_winner(), Some(TerminalClaimant::Reply));
        assert!(c.is_completed());
        // One-shot: no second commit, no reopen.
        assert!(!c.commit_terminal(&owner), "Completed is terminal");
        assert!(c.try_claim_timeout_terminal(&ident(1)).is_none());
    }

    #[test]
    fn only_one_claimant_wins_open() {
        let c = armed(1);
        let reply = c.try_claim_reply_terminal(&ident(1)).expect("reply wins");
        // Every other claimant loses while Reserved.
        assert!(c.try_claim_timeout_terminal(&ident(1)).is_none());
        assert!(c.try_claim_peer_death_terminal(&ident(1)).is_none());
        assert!(c.try_claim_caller_exit_terminal(&ident(1)).is_none());
        assert!(c.try_claim_endpoint_gone_terminal(&ident(1)).is_none());
        assert!(c.commit_terminal(&reply));
    }

    #[test]
    fn stale_generation_never_authorizes() {
        let c = armed(2);
        // Same numeric TIDs, WRONG record generation → refused.
        assert!(c.try_claim_reply_terminal(&ident(1)).is_none());
        // Same generation but a replacement replier ASID → refused.
        let mut wrong_asid = ident(2);
        wrong_asid.replier_asid = Asid(99);
        assert!(c.try_claim_reply_terminal(&wrong_asid).is_none());
        // Exact identity → allowed.
        assert!(c.try_claim_reply_terminal(&ident(2)).is_some());
    }

    #[test]
    fn release_reopens_only_for_the_owner_same_generation() {
        let c = armed(1);
        let reply = c
            .try_claim_reply_terminal(&ident(1))
            .expect("reply reserves");
        assert!(
            c.release_terminal_if_retryable(&reply),
            "owner releases Reserved → Open"
        );
        assert!(c.is_open(), "record reopened for retry");
        // A second release by the stale owner does nothing (already Open).
        assert!(!c.release_terminal_if_retryable(&reply));
        // A fresh claimant may now win at the same generation.
        let timeout = c
            .try_claim_timeout_terminal(&ident(1))
            .expect("timeout wins the reopened record");
        assert!(c.commit_terminal(&timeout));
        assert_eq!(c.committed_winner(), Some(TerminalClaimant::Timeout));
    }

    #[test]
    fn released_owner_cannot_commit_after_competitor_wins() {
        let c = armed(1);
        let reply = c
            .try_claim_reply_terminal(&ident(1))
            .expect("reply reserves");
        assert!(c.release_terminal_if_retryable(&reply));
        // Timeout wins the reopened record and commits.
        let timeout = c
            .try_claim_timeout_terminal(&ident(1))
            .expect("timeout wins");
        assert!(c.commit_terminal(&timeout));
        // The late reply owner cannot commit or restore over the timeout win.
        assert!(
            !c.commit_terminal(&reply),
            "a released owner cannot win after a competitor completed"
        );
        assert!(!c.release_terminal_if_retryable(&reply));
        assert_eq!(c.committed_winner(), Some(TerminalClaimant::Timeout));
    }

    #[test]
    fn rearm_bumps_epoch_and_invalidates_old_owner_token() {
        let mut c = armed(1);
        let stale = c
            .try_claim_reply_terminal(&ident(1))
            .expect("reserve gen 1");
        // Re-arm for a new record generation (new epoch). The stale owner token is
        // now unusable — it cannot commit or release the re-armed cell.
        c.arm(ident(2));
        assert!(c.is_open());
        assert!(
            !c.commit_terminal(&stale),
            "stale-epoch owner cannot commit"
        );
        assert!(!c.release_terminal_if_retryable(&stale));
        // A fresh claim on the new generation still works, with the SAME numeric
        // claimant tag — proving the epoch (not the tag) protects against ABA.
        let fresh = c
            .try_claim_reply_terminal(&ident(2))
            .expect("reserve gen 2");
        assert!(c.commit_terminal(&fresh));
        assert_eq!(c.committed_winner(), Some(TerminalClaimant::Reply));
    }

    #[test]
    fn non_owner_claimant_cannot_commit() {
        let c = armed(1);
        let reply = c
            .try_claim_reply_terminal(&ident(1))
            .expect("reply reserves");
        // Fabricate a different-claimant owner is impossible (private ctor); the
        // only owner in existence is `reply`. A different-tag commit encoding never
        // matches the Reserved(Reply) word.
        let forged = TerminalOwner {
            claimant: TerminalClaimant::Timeout,
            epoch: reply.epoch,
        };
        assert!(
            !c.commit_terminal(&forged),
            "wrong claimant tag fails closed"
        );
        assert!(c.commit_terminal(&reply), "the true owner still commits");
    }
}
