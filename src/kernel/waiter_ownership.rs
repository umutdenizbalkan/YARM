// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199D-WA2A — the generation-bearing endpoint-waiter ownership primitive.
//!
//! # Why
//!
//! `WAITER_OWNERSHIP_EXCLUSIVE=no`. Today the endpoint receive-waiter table is the *de facto*
//! arbiter between the paths that can wake a `Blocked(EndpointReceive)` task — but only for the
//! paths that happen to consult it. The census below shows two production owners that do not,
//! which is why the x86 direct production default is currently OFF
//! (`doc/KERNEL_UNLOCK_AUDIT.md` §6.1.29, §6.1.30).
//!
//! This module is the single typed primitive a later increment can route every one of those
//! paths through. **It is helper-only: zero production call sites in this increment.**
//!
//! # The census this primitive must eventually cover
//!
//! Every production path that installs/replaces/removes an endpoint receive waiter, or that
//! moves an endpoint-blocked task out of `Blocked`, together with the domains it holds and
//! whether it consults the waiter today:
//!
//! | # | function | file | lock domains | waiter identity | endpoint generation | consults waiter? |
//! |---|---|---|---|---|---|---|
//! | 1 | `publish_recv_waiter_live` | `ipc_state.rs:6079` | ipc(3) | exact `{tid,asid}` | yes (slot) | installs (displacement reported) |
//! | 2 | `try_publish_recv_waiter_audit_only` | `ipc_state.rs:6024` | ipc(3) | exact | yes | installs |
//! | 3 | `sr_claim_endpoint_waiter_split` | `runtime.rs:3961` | ipc(3) | exact | **yes, compared** | claims exactly once |
//! | 4 | `sr_restore_endpoint_waiter_split` | `runtime.rs:4031` | task(2) probe → ipc(3) | exact | yes | restores exact identity |
//! | 5 | `ipc_try_send_to_plain_receiver_endpoint_only` | `ipc_state.rs:4339` | ipc(3) | exact | yes | clears iff identity matches |
//! | 6 | `ipc_try_send_sync_endpoint_only` | `ipc_state.rs:5895` | ipc(3) | exact | yes | clears iff identity matches |
//! | 7 | `ipc_clear_plain_receiver_waiter_only` | `ipc_state.rs:5833` | ipc(3) | exact | yes | clears iff identity matches |
//! | 8 | `wake_waiter_for_endpoint` | `ipc_state.rs:3489` | ipc(3) → task(2) → sched(1) | index only | slot | takes unconditionally |
//! | 9 | `ipc_reply` | `ipc_state.rs:5240` | broad | index only | slot | takes unconditionally |
//! | 10 | `ctx_finalize_and_wake` (shared region) | `shared_region_txn.rs:935` | ipc(3) | index only | slot | takes unconditionally |
//! | 11 | `clear_ipc_waiters_for_tid` (teardown) | `ipc_state.rs:2751` | ipc(3) | identity sweep | — | clears by identity, all slots |
//! | 12 | **`process_ipc_timeout_deadlines`** | `ipc_state.rs:3621` | task(2) **then** ipc(3) | identity sweep | — | **NO — wakes at task rank first** |
//! | 13 | **notification signal wake** | `ipc_state.rs:5430` | ipc(3) → task(2) → sched(1) | TID only | — | **NO — different table** |
//! | 14 | server-death completion | `ipc_state.rs:516` | terminal → ipc(3) → task(2) | exact + blocked-recv generation | yes | claims terminal *and* waiter |
//! | 15 | reply-receive timeout | `ipc_state.rs` (token path) | terminal → ipc(3) → task(2) | exact + blocked-recv generation | yes | claims terminal *and* waiter |
//!
//! Rows **12** and **13** are the exclusivity break. Rows 8, 9 and 10 take by index without an
//! identity compare, so they cannot distinguish a replacement waiter from the one they meant.
//!
//! # What this primitive adds
//!
//! A key that is exact in **four** dimensions rather than the table's two: endpoint index **and
//! generation**, waiter tid **and** asid, plus the task's blocked-wait generation. A recycled
//! endpoint slot, a reused numeric TID with a new ASID, and a task that unblocked and reblocked
//! are all different keys, so none of them can inherit another's claim.
//!
//! # Lock discipline
//!
//! This module acquires **nothing**. It is a pure state machine over owned data, so it cannot
//! nest a task or scheduler lock beneath the ipc lock — the caller supplies the rank-3 guard and
//! the primitive borrows `&mut WaiterOwnershipTable` inside it. [`WaiterClaimToken`] is owned and
//! `Copy`, so it survives the guard being dropped.

// HELPER-ONLY marker, matching the split-seam convention elsewhere in the tree: this primitive
// has ZERO production call sites in this increment, so every item is dead outside tests. The
// attribute is the honest statement of that, and `the_primitive_has_no_production_caller` is what
// enforces it — remove the attribute only in the increment that wires the first real caller.
#![cfg_attr(not(test), allow(dead_code))]

use crate::kernel::boot::ReceiverWaiterIdentity;

/// How many concurrent claims the table can hold. Bounded by construction: the primitive
/// allocates nothing and exhaustion is a typed, fail-closed error rather than a growth event.
pub(crate) const WAITER_OWNERSHIP_SLOTS: usize = 64;

/// Which subsystem owns a claim. Naming an owner here does **not** wire that path — every one
/// of these is helper-only in this increment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaiterOwner {
    /// The off-lock NR6 direct request transaction.
    DirectRequest,
    /// The off-lock NR7 direct reply transaction.
    DirectReply,
    /// The ordinary (non-token-bearing) IPC deadline scan — census row 12.
    OrdinaryTimeout,
    /// Legacy in-lock endpoint delivery: send-to-blocked-receiver, `ipc_reply`, shared region.
    LegacyDelivery,
    /// Notification signal delivery — census row 13.
    Notification,
    /// Task teardown / exit / replacement sweeps.
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

/// The slot state machine. Typed, not booleans: stale, duplicate, recycled-generation and
/// foreign-owner outcomes are each separately observable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaiterOwnership {
    /// No live claim. A fresh claim may take it.
    Available,
    /// Owned. Only this exact `{owner, claim_generation}` may settle it.
    Claimed {
        owner: WaiterOwner,
        claim_generation: u64,
    },
    /// Terminal: the claim was settled as a completed delivery/wake. Nothing may re-arm it.
    Consumed,
    /// Terminal: the claim was abandoned without delivery. Nothing may re-arm it.
    Cancelled,
}

/// An owned proof of ownership. `Copy` and self-contained, so it survives the rank-3 guard being
/// released — which is the whole point: the owner does its user copies and cap work off-lock and
/// settles later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WaiterClaimToken {
    pub(crate) key: WaiterKey,
    pub(crate) owner: WaiterOwner,
    pub(crate) claim_generation: u64,
}

/// Why a claim was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimError {
    /// Someone already owns it. The current owner is reported so a duplicate claim by the SAME
    /// owner is distinguishable from a genuine foreign collision.
    AlreadyClaimed { by: WaiterOwner },
    /// Terminal — already delivered.
    Consumed,
    /// Terminal — already abandoned.
    Cancelled,
    /// The table is full. Fail closed; nothing is evicted.
    CapacityExhausted,
}

/// Why a settle (consume / restore / cancel) was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettleError {
    /// No slot holds this exact key — a recycled endpoint generation, a replacement task, or a
    /// re-blocked task all land here, which is precisely what stops a stale restore.
    NoSuchClaim,
    /// The slot is claimed, but by a different owner.
    ForeignOwner { by: WaiterOwner },
    /// The right owner, but an older claim than the live one: a stale token.
    StaleClaimGeneration { live: u64 },
    /// The slot is not currently claimed.
    NotClaimed { state: WaiterOwnership },
}

/// A bounded, generation-bearing ownership table. Holds no lock: mutation happens under the
/// caller's ipc rank-3 guard, and nothing is ever nested beneath it.
#[derive(Debug)]
pub(crate) struct WaiterOwnershipTable {
    slots: [Option<(WaiterKey, WaiterOwnership)>; WAITER_OWNERSHIP_SLOTS],
    /// Strictly increasing. Stamped into every claim so a token from an earlier claim of the
    /// same key is recognisably stale even after the slot returns to `Available`.
    next_claim_generation: u64,
}

impl Default for WaiterOwnershipTable {
    fn default() -> Self {
        Self::new()
    }
}

impl WaiterOwnershipTable {
    pub(crate) const fn new() -> Self {
        Self {
            slots: [None; WAITER_OWNERSHIP_SLOTS],
            next_claim_generation: 1,
        }
    }

    fn find(&self, key: &WaiterKey) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.as_ref().is_some_and(|(k, _)| k == key))
    }

    /// Read the state for an exact key. `Available` for a key the table has never seen — an
    /// unseen key is claimable, which is the same observable answer.
    pub(crate) fn state(&self, key: &WaiterKey) -> WaiterOwnership {
        match self.find(key) {
            Some(i) => self.slots[i].expect("found").1,
            None => WaiterOwnership::Available,
        }
    }

    /// Claim the exact incarnation for `owner`.
    ///
    /// Fails closed on every ambiguity: an existing claim (reporting its owner, so a duplicate by
    /// the same owner is distinguishable), either terminal state, or a full table.
    pub(crate) fn claim(
        &mut self,
        key: WaiterKey,
        owner: WaiterOwner,
    ) -> Result<WaiterClaimToken, ClaimError> {
        if let Some(i) = self.find(&key) {
            match self.slots[i].expect("found").1 {
                WaiterOwnership::Claimed { owner: by, .. } => {
                    return Err(ClaimError::AlreadyClaimed { by });
                }
                WaiterOwnership::Consumed => return Err(ClaimError::Consumed),
                WaiterOwnership::Cancelled => return Err(ClaimError::Cancelled),
                WaiterOwnership::Available => {
                    let claim_generation = self.stamp();
                    self.slots[i] = Some((
                        key,
                        WaiterOwnership::Claimed {
                            owner,
                            claim_generation,
                        },
                    ));
                    return Ok(WaiterClaimToken {
                        key,
                        owner,
                        claim_generation,
                    });
                }
            }
        }
        let Some(free) = self.slots.iter().position(Option::is_none) else {
            return Err(ClaimError::CapacityExhausted);
        };
        let claim_generation = self.stamp();
        self.slots[free] = Some((
            key,
            WaiterOwnership::Claimed {
                owner,
                claim_generation,
            },
        ));
        Ok(WaiterClaimToken {
            key,
            owner,
            claim_generation,
        })
    }

    fn stamp(&mut self) -> u64 {
        let g = self.next_claim_generation;
        self.next_claim_generation = self.next_claim_generation.wrapping_add(1);
        g
    }

    /// Validate a token against the live slot. Every dimension is compared: the key (all four of
    /// its fields, by `PartialEq`), the owner, and the claim generation.
    fn validate(&self, token: &WaiterClaimToken) -> Result<usize, SettleError> {
        let Some(i) = self.find(&token.key) else {
            return Err(SettleError::NoSuchClaim);
        };
        match self.slots[i].expect("found").1 {
            WaiterOwnership::Claimed {
                owner,
                claim_generation,
            } => {
                if owner != token.owner {
                    return Err(SettleError::ForeignOwner { by: owner });
                }
                if claim_generation != token.claim_generation {
                    return Err(SettleError::StaleClaimGeneration {
                        live: claim_generation,
                    });
                }
                Ok(i)
            }
            state => Err(SettleError::NotClaimed { state }),
        }
    }

    /// Terminal success: the owner delivered, so nothing may re-arm this incarnation.
    pub(crate) fn consume(&mut self, token: &WaiterClaimToken) -> Result<(), SettleError> {
        let i = self.validate(token)?;
        self.slots[i] = Some((token.key, WaiterOwnership::Consumed));
        Ok(())
    }

    /// Non-terminal rollback: the owner did not deliver, so the exact incarnation becomes
    /// claimable again. Requires the exact endpoint generation, waiter incarnation, task-wait
    /// generation, owner and claim generation — so a stale token can never recreate a waiter in a
    /// recycled endpoint slot.
    pub(crate) fn restore(&mut self, token: &WaiterClaimToken) -> Result<(), SettleError> {
        let i = self.validate(token)?;
        self.slots[i] = Some((token.key, WaiterOwnership::Available));
        Ok(())
    }

    /// Terminal abandonment: the incarnation is dead (task exited, endpoint destroyed) and must
    /// never be claimed again.
    pub(crate) fn cancel(&mut self, token: &WaiterClaimToken) -> Result<(), SettleError> {
        let i = self.validate(token)?;
        self.slots[i] = Some((token.key, WaiterOwnership::Cancelled));
        Ok(())
    }

    /// Live (non-terminal, claimed) entries. Diagnostic only.
    pub(crate) fn claimed_count(&self) -> usize {
        self.slots
            .iter()
            .flatten()
            .filter(|(_, s)| matches!(s, WaiterOwnership::Claimed { .. }))
            .count()
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

    // ── One winner ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn exactly_one_of_two_competing_owners_wins() {
        let mut t = WaiterOwnershipTable::new();
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
        // …and only the winner can settle.
        assert!(t.consume(&token).is_ok());
    }

    #[test]
    fn a_duplicate_claim_by_the_same_owner_is_refused_and_names_itself() {
        let mut t = WaiterOwnershipTable::new();
        let first = t.claim(base(), WaiterOwner::DirectReply).expect("first");
        assert_eq!(
            t.claim(base(), WaiterOwner::DirectReply),
            Err(ClaimError::AlreadyClaimed {
                by: WaiterOwner::DirectReply
            }),
            "a duplicate is refused — it must not silently re-stamp a fresh generation"
        );
        // The original token is still the live one.
        assert!(t.restore(&first).is_ok());
    }

    // ── Foreign owner ───────────────────────────────────────────────────────────────────────

    #[test]
    fn a_foreign_owner_can_neither_consume_nor_restore() {
        let mut t = WaiterOwnershipTable::new();
        let real = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        let forged = WaiterClaimToken {
            owner: WaiterOwner::Notification,
            ..real
        };
        assert_eq!(
            t.consume(&forged),
            Err(SettleError::ForeignOwner {
                by: WaiterOwner::DirectRequest
            })
        );
        assert_eq!(
            t.restore(&forged),
            Err(SettleError::ForeignOwner {
                by: WaiterOwner::DirectRequest
            })
        );
        assert_eq!(
            t.cancel(&forged),
            Err(SettleError::ForeignOwner {
                by: WaiterOwner::DirectRequest
            })
        );
        assert_eq!(
            t.state(&base()),
            WaiterOwnership::Claimed {
                owner: WaiterOwner::DirectRequest,
                claim_generation: real.claim_generation
            },
            "a rejected settle mutates nothing"
        );
    }

    // ── The three identity/generation dimensions ────────────────────────────────────────────

    #[test]
    fn a_recycled_endpoint_generation_is_a_different_incarnation() {
        let mut t = WaiterOwnershipTable::new();
        let token = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        // Same index, endpoint destroyed and recreated.
        let recycled = WaiterKey {
            endpoint_generation: base().endpoint_generation + 1,
            ..base()
        };
        assert!(
            t.claim(recycled, WaiterOwner::LegacyDelivery).is_ok(),
            "the new incarnation is independently claimable"
        );
        // A token for the OLD incarnation must not settle the new one.
        let stale = WaiterClaimToken {
            key: recycled,
            ..token
        };
        assert_eq!(
            t.restore(&stale),
            Err(SettleError::ForeignOwner {
                by: WaiterOwner::LegacyDelivery
            }),
            "a stale token can never restore into a recycled endpoint slot"
        );
    }

    #[test]
    fn tid_reuse_with_a_changed_asid_is_a_different_incarnation() {
        let mut t = WaiterOwnershipTable::new();
        let _ = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        let replacement = key(3, 7, 42, 2 ^ 0x55, 11); // same numeric TID, new ASID
        assert!(
            t.claim(replacement, WaiterOwner::LegacyDelivery).is_ok(),
            "a replacement task at the same TID is a different key"
        );
        assert_eq!(t.claimed_count(), 2, "the two incarnations coexist");
    }

    #[test]
    fn a_changed_blocked_wait_generation_is_a_different_incarnation() {
        let mut t = WaiterOwnershipTable::new();
        let token = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        // The task unblocked, ran, and reblocked on the same endpoint.
        let reblocked = WaiterKey {
            wait_generation: base().wait_generation + 1,
            ..base()
        };
        assert_eq!(
            t.state(&reblocked),
            WaiterOwnership::Available,
            "the new wait is unclaimed"
        );
        let stale = WaiterClaimToken {
            key: reblocked,
            ..token
        };
        assert_eq!(
            t.restore(&stale),
            Err(SettleError::NoSuchClaim),
            "a token from the previous wait cannot settle the new one"
        );
    }

    // ── Settle semantics ────────────────────────────────────────────────────────────────────

    #[test]
    fn restore_after_an_exact_non_terminal_rollback_makes_it_claimable_again() {
        let mut t = WaiterOwnershipTable::new();
        let token = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        assert_eq!(t.restore(&token), Ok(()));
        assert_eq!(t.state(&base()), WaiterOwnership::Available);
        // A second owner may now take the same exact incarnation.
        let second = t
            .claim(base(), WaiterOwner::LegacyDelivery)
            .expect("reclaim");
        assert_ne!(
            second.claim_generation, token.claim_generation,
            "the reclaim gets a fresh generation, so the old token is now stale"
        );
        assert_eq!(
            t.restore(&token),
            Err(SettleError::ForeignOwner {
                by: WaiterOwner::LegacyDelivery
            }),
            "the settled token cannot settle the new claim"
        );
    }

    /// **The claim generation alone must reject a stale token.** Re-claimed by the *same* owner,
    /// so the owner comparison cannot mask it — only `claim_generation` distinguishes the old
    /// token from the live one. Without this, a settled owner could settle somebody else's
    /// later claim of the same incarnation.
    #[test]
    fn a_stale_token_is_rejected_even_when_the_same_owner_reclaims() {
        let mut t = WaiterOwnershipTable::new();
        let first = t.claim(base(), WaiterOwner::DirectRequest).expect("claim");
        assert_eq!(t.restore(&first), Ok(()));
        let second = t
            .claim(base(), WaiterOwner::DirectRequest)
            .expect("same owner reclaims the same incarnation");
        assert_ne!(second.claim_generation, first.claim_generation);
        for (what, r) in [
            ("consume", t.consume(&first)),
            ("restore", t.restore(&first)),
            ("cancel", t.cancel(&first)),
        ] {
            assert_eq!(
                r,
                Err(SettleError::StaleClaimGeneration {
                    live: second.claim_generation
                }),
                "{what} with a stale token must be refused on the claim generation alone"
            );
        }
        assert_eq!(
            t.state(&base()),
            WaiterOwnership::Claimed {
                owner: WaiterOwner::DirectRequest,
                claim_generation: second.claim_generation
            },
            "and the live claim is untouched"
        );
        assert_eq!(t.consume(&second), Ok(()), "the live token still works");
    }

    #[test]
    fn restore_after_consume_is_rejected() {
        let mut t = WaiterOwnershipTable::new();
        let token = t.claim(base(), WaiterOwner::DirectReply).expect("claim");
        assert_eq!(t.consume(&token), Ok(()));
        assert_eq!(
            t.restore(&token),
            Err(SettleError::NotClaimed {
                state: WaiterOwnership::Consumed
            }),
            "a delivered incarnation must never be re-armed"
        );
        assert_eq!(
            t.claim(base(), WaiterOwner::Teardown),
            Err(ClaimError::Consumed)
        );
    }

    #[test]
    fn cancellation_is_terminal() {
        let mut t = WaiterOwnershipTable::new();
        let token = t.claim(base(), WaiterOwner::Teardown).expect("claim");
        assert_eq!(t.cancel(&token), Ok(()));
        assert_eq!(t.state(&base()), WaiterOwnership::Cancelled);
        assert_eq!(
            t.claim(base(), WaiterOwner::DirectRequest),
            Err(ClaimError::Cancelled)
        );
        assert_eq!(
            t.restore(&token),
            Err(SettleError::NotClaimed {
                state: WaiterOwnership::Cancelled
            })
        );
        assert_eq!(
            t.consume(&token),
            Err(SettleError::NotClaimed {
                state: WaiterOwnership::Cancelled
            })
        );
    }

    // ── Bounded, fail-closed ────────────────────────────────────────────────────────────────

    #[test]
    fn table_capacity_exhaustion_fails_closed() {
        let mut t = WaiterOwnershipTable::new();
        for i in 0..WAITER_OWNERSHIP_SLOTS {
            t.claim(key(i, 1, i as u64, 1, 1), WaiterOwner::DirectRequest)
                .unwrap_or_else(|e| panic!("slot {i} must claim: {e:?}"));
        }
        assert_eq!(
            t.claim(key(9999, 1, 9999, 1, 1), WaiterOwner::DirectRequest),
            Err(ClaimError::CapacityExhausted),
            "exhaustion is typed and fail-closed — nothing is evicted"
        );
        assert_eq!(
            t.claimed_count(),
            WAITER_OWNERSHIP_SLOTS,
            "and no existing claim was disturbed"
        );
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
            "with_ipc_split_mut",
            "with_task_tcbs_split_mut",
            "with_scheduler_split_mut",
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
        ] {
            assert!(
                !code.contains(forbidden),
                "the primitive must not name `{forbidden}`:\n{code}"
            );
        }
    }
}
