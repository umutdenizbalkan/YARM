// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 200B — bounded, generation-bearing DEADLINE TOKEN mechanism for reply
//! receives.
//!
//! A deadline token may *request* a timeout terminal claim, but it is NEVER a
//! second terminal-result authority. [`crate::kernel::terminal_ownership::TerminalCell`]
//! remains the sole arbiter among Reply, Timeout, PeerDeath, CallerExit and
//! EndpointGone. A token only decides "may a fire attempt run"; the fire owner
//! then calls `try_claim_timeout_terminal` against the SAME `TerminalCell`, and
//! that CAS decides whether timeout won. This stage wires NO real timer, no
//! production deadline queue, no caller wake, no task-exit scan and no live
//! timeout marker — it is a hosted registration/ownership mechanism.
//!
//! ## Lifecycle
//!
//! ```text
//!   Vacant → Armed → ClaimedForFire → Disarmed | Completed
//!              │                          ▲
//!              └── cancel / terminal-disarm┘  (Armed → Disarmed)
//! ```
//!
//! * `arm` — reserve a token slot for a registration; publish `Armed` (Release).
//! * `claim_fire` — a synthetic hosted fire; `Armed → ClaimedForFire` (one owner).
//! * `complete_fire` — fire won the timeout terminal; `ClaimedForFire → Completed`.
//! * `disarm_fire` — fire lost the timeout terminal; `ClaimedForFire → Disarmed`.
//! * `restore_fire_claim_if_retryable` — retryable rollback; `ClaimedForFire → Armed`.
//! * `cancel_exact` / `disarm_after_terminal_completion` — `Armed → Disarmed`.
//!
//! ## Generation-bearing identity
//!
//! A [`DeadlineTokenIdentity`] carries the token slot + generation, the terminal
//! epoch, AND the full [`TerminalIdentity`] — so a fire claim must match the
//! CURRENT terminal identity exactly. Numeric TID, reply-record index or endpoint
//! index alone NEVER authorize a timeout claim: a reused TID (different ASID), a
//! reused reply slot (advanced generation/epoch), or a replaced endpoint
//! generation all mismatch the token identity and are refused.
//!
//! ## Memory ordering
//!
//! `arm` writes the immutable token identity FIRST, then publishes `Armed` with a
//! single `Release` store LAST. A fire claimant `Acquire`-loads the state first, so
//! observing `Armed` implies the complete token identity is visible. The fire claim
//! is a `compare_exchange(ARMED, CLAIMED_FOR_FIRE, AcqRel, Acquire)`; every other
//! transition is an AcqRel/Acquire CAS from the exact `{epoch}`. A per-cell `epoch`
//! (ABA nonce, bumped on every arm) prevents an old cancel/restore from re-arming a
//! newly published token. Identity fields are only ever written under `&mut self`
//! (exclusive), so there is no concurrent non-atomic identity read/write.

use crate::kernel::terminal_ownership::TerminalIdentity;
use core::sync::atomic::{AtomicU64, Ordering};

/// Generation-bearing identity of a deadline registration for one blocked reply
/// receive. Immutable per arming; a fire/cancel/disarm must present an EXACT `==`
/// match. It embeds the full [`TerminalIdentity`] plus the terminal epoch, so the
/// token can only act on the CURRENT terminal incarnation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeadlineTokenIdentity {
    /// The bounded deadline-registration store slot.
    pub token_index: usize,
    /// The token's own generation (bumped per registration for a slot).
    pub token_generation: u64,
    /// The terminal cell epoch this registration is bound to (ABA-safe binding).
    pub terminal_epoch: u64,
    /// The exact terminal identity that must still hold for a timeout claim.
    pub terminal_identity: TerminalIdentity,
}

impl DeadlineTokenIdentity {
    /// The zeroed identity a vacant token slot carries; matches no live token.
    pub const ZERO: Self = Self {
        token_index: usize::MAX,
        token_generation: 0,
        terminal_epoch: 0,
        terminal_identity: TerminalIdentity::ZERO,
    };
}

/// A deterministic failure to arm a deadline token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineArmError {
    /// An active registration already exists for this reply receive (a second arm
    /// for the same registration is refused — never silently overwritten).
    AlreadyArmed,
    /// The bounded deadline-registration store has no free slot.
    StoreFull,
    /// The terminal cell is no longer `Open` / its identity/epoch drifted, so the
    /// registration cannot be armed (terminal ownership changed before publication).
    TerminalNotOpen,
}

// ── State-word packing: [ epoch (61) | phase (3) ] ─────────────────────────────
const DL_PHASE_MASK: u64 = 0b111;
const DL_EPOCH_SHIFT: u64 = 3;

const DL_VACANT: u64 = 0;
const DL_ARMED: u64 = 1;
const DL_CLAIMED_FOR_FIRE: u64 = 2;
const DL_DISARMED: u64 = 3;
const DL_COMPLETED: u64 = 4;

#[inline]
const fn dl_encode(epoch: u64, phase: u64) -> u64 {
    (epoch << DL_EPOCH_SHIFT) | phase
}
#[inline]
const fn dl_phase(word: u64) -> u64 {
    word & DL_PHASE_MASK
}
#[inline]
const fn dl_epoch(word: u64) -> u64 {
    word >> DL_EPOCH_SHIFT
}

/// The handle minted by a successful [`DeadlineTokenCell::arm`]. Carries the exact
/// token identity + epoch a fire/cancel must present. Private fields: only `arm`
/// mints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeadlineTokenHandle {
    identity: DeadlineTokenIdentity,
    epoch: u64,
}

impl DeadlineTokenHandle {
    #[inline]
    pub const fn identity(&self) -> &DeadlineTokenIdentity {
        &self.identity
    }
    #[inline]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }
    #[inline]
    pub const fn token_index(&self) -> usize {
        self.identity.token_index
    }
    #[inline]
    pub const fn token_generation(&self) -> u64 {
        self.identity.token_generation
    }
}

/// The exclusive fire-ownership token minted by a successful `claim_fire`. Only its
/// holder can complete, disarm or restore the fire. Cannot be constructed outside
/// this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeadlineFireOwner {
    epoch: u64,
}

/// One bounded deadline-registration cell. Its immutable-per-arming identity is
/// guarded by `&mut self` at arm time; only the packed atomic state transitions
/// after publication.
#[derive(Debug)]
pub struct DeadlineTokenCell {
    identity: DeadlineTokenIdentity,
    state: AtomicU64,
}

impl Default for DeadlineTokenCell {
    fn default() -> Self {
        Self::vacant()
    }
}

impl DeadlineTokenCell {
    /// A vacant token slot (epoch 0, `Vacant`, zeroed identity). Const so a whole
    /// `[DeadlineTokenCell; N]` registration store can be initialized at boot.
    pub const fn vacant() -> Self {
        Self {
            identity: DeadlineTokenIdentity::ZERO,
            state: AtomicU64::new(dl_encode(0, DL_VACANT)),
        }
    }

    /// Arm this slot for a registration. Succeeds only from a recyclable phase
    /// (`Vacant`, `Disarmed`, `Completed`); an already-`Armed`/`ClaimedForFire`
    /// slot is refused (`None` ⇒ the caller reports `AlreadyArmed`). Bumps the
    /// epoch, writes the complete identity FIRST, then publishes `Armed` with a
    /// single `Release` store LAST. Requires `&mut self` (exclusive): no partially
    /// armed token is ever visible, and the identity rewrite cannot race a claim.
    pub fn arm(&mut self, identity: DeadlineTokenIdentity) -> Option<DeadlineTokenHandle> {
        let cur = self.state.load(Ordering::Relaxed);
        match dl_phase(cur) {
            DL_ARMED | DL_CLAIMED_FOR_FIRE => return None, // already armed — no overwrite
            _ => {}
        }
        let mut epoch = dl_epoch(cur).wrapping_add(1);
        if epoch == 0 {
            epoch = 1;
        }
        self.identity = identity;
        self.state
            .store(dl_encode(epoch, DL_ARMED), Ordering::Release);
        Some(DeadlineTokenHandle { identity, epoch })
    }

    #[inline]
    pub fn identity(&self) -> &DeadlineTokenIdentity {
        &self.identity
    }

    #[inline]
    pub fn current_epoch(&self) -> u64 {
        dl_epoch(self.state.load(Ordering::Acquire))
    }

    #[inline]
    pub fn is_armed(&self) -> bool {
        dl_phase(self.state.load(Ordering::Acquire)) == DL_ARMED
    }
    #[inline]
    pub fn is_fire_claimed(&self) -> bool {
        dl_phase(self.state.load(Ordering::Acquire)) == DL_CLAIMED_FOR_FIRE
    }
    #[inline]
    pub fn is_completed(&self) -> bool {
        dl_phase(self.state.load(Ordering::Acquire)) == DL_COMPLETED
    }
    #[inline]
    pub fn is_disarmed(&self) -> bool {
        dl_phase(self.state.load(Ordering::Acquire)) == DL_DISARMED
    }
    /// `true` iff the slot can accept a fresh registration (recyclable/free).
    #[inline]
    pub fn is_free(&self) -> bool {
        matches!(
            dl_phase(self.state.load(Ordering::Acquire)),
            DL_VACANT | DL_DISARMED | DL_COMPLETED
        )
    }

    /// Claim the synthetic hosted fire for an armed token: `Armed → ClaimedForFire`
    /// with a single AcqRel CAS. Exactly one fire owner wins; a duplicate fire, a
    /// stale handle (wrong epoch/identity), or a cancelled/completed token all get
    /// `None` and mutate nothing. The fire owner MUST then attempt
    /// `try_claim_timeout_terminal` against the terminal cell — the token does NOT
    /// itself decide that timeout won.
    #[must_use]
    pub fn claim_fire(&self, handle: &DeadlineTokenHandle) -> Option<DeadlineFireOwner> {
        let w = self.state.load(Ordering::Acquire);
        if dl_phase(w) != DL_ARMED {
            return None;
        }
        if self.identity != handle.identity {
            return None; // stale/mismatched registration identity
        }
        let epoch = dl_epoch(w);
        if epoch != handle.epoch {
            return None;
        }
        match self.state.compare_exchange(
            dl_encode(epoch, DL_ARMED),
            dl_encode(epoch, DL_CLAIMED_FOR_FIRE),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Some(DeadlineFireOwner { epoch }),
            Err(_) => None,
        }
    }

    /// Exact cancellation of an armed (not-yet-fired) token: `Armed → Disarmed`.
    /// Requires the exact handle identity + epoch, so a stale cancel cannot cancel a
    /// newer registration. A late fire after cancellation mutates nothing.
    #[must_use]
    pub fn cancel_exact(&self, handle: &DeadlineTokenHandle) -> bool {
        self.disarm_armed_exact(&handle.identity, handle.epoch)
    }

    /// Disarm an armed token because a NON-timeout terminal (Reply / PeerDeath /
    /// CallerExit / EndpointGone) obtained terminal ownership: `Armed → Disarmed`.
    /// Requires the exact identity + epoch — a stale disarm must not cancel a newer
    /// registration.
    #[must_use]
    pub fn disarm_after_terminal_completion(
        &self,
        expect: &DeadlineTokenIdentity,
        expect_epoch: u64,
    ) -> bool {
        self.disarm_armed_exact(expect, expect_epoch)
    }

    fn disarm_armed_exact(&self, expect: &DeadlineTokenIdentity, expect_epoch: u64) -> bool {
        let w = self.state.load(Ordering::Acquire);
        if dl_phase(w) != DL_ARMED {
            return false;
        }
        if self.identity != *expect || dl_epoch(w) != expect_epoch {
            return false;
        }
        self.state
            .compare_exchange(
                dl_encode(expect_epoch, DL_ARMED),
                dl_encode(expect_epoch, DL_DISARMED),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// The fire owner won the timeout terminal claim: `ClaimedForFire → Completed`.
    /// Only the exact fire owner succeeds.
    #[must_use]
    pub fn complete_fire(&self, owner: &DeadlineFireOwner) -> bool {
        self.state
            .compare_exchange(
                dl_encode(owner.epoch, DL_CLAIMED_FOR_FIRE),
                dl_encode(owner.epoch, DL_COMPLETED),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// The fire owner LOST the timeout terminal claim (a non-timeout terminal
    /// already won): `ClaimedForFire → Disarmed`. No terminal release, no result
    /// copy, no wake. Only the exact fire owner succeeds.
    #[must_use]
    pub fn disarm_fire(&self, owner: &DeadlineFireOwner) -> bool {
        self.state
            .compare_exchange(
                dl_encode(owner.epoch, DL_CLAIMED_FOR_FIRE),
                dl_encode(owner.epoch, DL_DISARMED),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Retryable rollback of a fire claim (e.g. a transient pre-terminal-claim
    /// failure while the registration is still valid): `ClaimedForFire → Armed`.
    /// Only the exact fire owner succeeds.
    #[must_use]
    pub fn restore_fire_claim_if_retryable(&self, owner: &DeadlineFireOwner) -> bool {
        self.state
            .compare_exchange(
                dl_encode(owner.epoch, DL_CLAIMED_FOR_FIRE),
                dl_encode(owner.epoch, DL_ARMED),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::terminal_ownership::TerminalIdentity;
    use crate::kernel::vm::Asid;

    fn terminal_ident(record_gen: u64) -> TerminalIdentity {
        TerminalIdentity {
            reply_record_index: 2,
            reply_record_generation: record_gen,
            caller_tid: ThreadId(1),
            caller_asid: Asid(11),
            replier_tid: ThreadId(2),
            replier_asid: Asid(22),
            reply_endpoint_index: 7,
            reply_endpoint_generation: 5,
            blocked_recv_generation: 9,
            deadline_token_generation: Some(1),
        }
    }

    fn token_ident(token_gen: u64, terminal_epoch: u64, record_gen: u64) -> DeadlineTokenIdentity {
        DeadlineTokenIdentity {
            token_index: 0,
            token_generation: token_gen,
            terminal_epoch,
            terminal_identity: terminal_ident(record_gen),
        }
    }

    fn armed(token_gen: u64) -> (DeadlineTokenCell, DeadlineTokenHandle) {
        let mut c = DeadlineTokenCell::vacant();
        let h = c.arm(token_ident(token_gen, 1, 1)).expect("arm");
        (c, h)
    }

    #[test]
    fn arm_publishes_armed_and_refuses_second_arm() {
        let mut c = DeadlineTokenCell::vacant();
        assert!(c.is_free());
        let _h = c.arm(token_ident(1, 1, 1)).expect("first arm");
        assert!(c.is_armed());
        // A second arm while Armed is refused (AlreadyArmed) — never overwritten.
        assert!(c.arm(token_ident(2, 1, 1)).is_none());
        assert!(c.is_armed());
    }

    #[test]
    fn single_fire_owner_then_complete() {
        let (c, h) = armed(1);
        let owner = c.claim_fire(&h).expect("fire claims armed token");
        assert!(c.is_fire_claimed());
        // A duplicate fire fails (already claimed).
        assert!(c.claim_fire(&h).is_none());
        assert!(c.complete_fire(&owner), "fire owner completes");
        assert!(c.is_completed());
        // No second completion.
        assert!(!c.complete_fire(&owner));
    }

    #[test]
    fn fire_loss_disarms_without_mutation() {
        let (c, h) = armed(1);
        let owner = c.claim_fire(&h).expect("fire claims");
        // Model: timeout lost the terminal → disarm the fire.
        assert!(c.disarm_fire(&owner));
        assert!(c.is_disarmed());
        assert!(!c.complete_fire(&owner), "cannot complete after disarm");
    }

    #[test]
    fn cancel_then_late_fire_mutates_nothing() {
        let (c, h) = armed(1);
        assert!(c.cancel_exact(&h), "exact cancel Armed → Disarmed");
        assert!(c.is_disarmed());
        // A late fire after cancellation mutates nothing.
        assert!(c.claim_fire(&h).is_none());
        assert!(c.is_disarmed());
    }

    #[test]
    fn stale_epoch_fire_and_disarm_rejected_after_rearm() {
        let mut c = DeadlineTokenCell::vacant();
        let stale = c.arm(token_ident(1, 1, 1)).expect("arm gen 1");
        // Disarm (terminal completion) then recycle the slot for a new registration.
        assert!(c.disarm_after_terminal_completion(stale.identity(), stale.epoch()));
        let fresh = c.arm(token_ident(2, 1, 1)).expect("re-arm gen 2");
        // The stale handle cannot fire or disarm the freshly-armed token.
        assert!(c.claim_fire(&stale).is_none(), "stale-epoch fire rejected");
        assert!(
            !c.disarm_after_terminal_completion(stale.identity(), stale.epoch()),
            "stale disarm cannot cancel the newer registration"
        );
        // The fresh handle still works.
        assert!(c.claim_fire(&fresh).is_some());
    }

    #[test]
    fn restore_fire_claim_reopens_for_retry() {
        let (c, h) = armed(1);
        let owner = c.claim_fire(&h).expect("fire claims");
        assert!(
            c.restore_fire_claim_if_retryable(&owner),
            "retryable rollback"
        );
        assert!(c.is_armed(), "token reopened for retry");
        // A fresh fire can claim again.
        assert!(c.claim_fire(&h).is_some());
    }

    #[test]
    fn mismatched_identity_fire_rejected() {
        let (c, _h) = armed(1);
        // A handle for a different terminal identity (reused caller TID, new ASID)
        // cannot fire the real token. Handle fields are private, so mint the
        // mismatched handle by arming a separate cell.
        let mut other = DeadlineTokenCell::vacant();
        let mut mismatched_ident = token_ident(1, 1, 1);
        mismatched_ident.terminal_identity.caller_asid = Asid(99);
        let wrong = other.arm(mismatched_ident).expect("arm other");
        assert!(c.claim_fire(&wrong).is_none(), "identity mismatch rejected");
        assert!(c.is_armed(), "the real token is untouched");
    }
}
