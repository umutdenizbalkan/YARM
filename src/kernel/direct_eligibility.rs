// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199D — the **direct-IPC eligibility contract**.
//!
//! Whether an `IpcCall` (NR6) or `IpcReply` (NR7) may be serviced off-lock used to be an
//! open-coded chain of `?` and early `return None` in the split helpers. This module makes
//! the decision one **pure, exhaustive** classification over facts the caller has already
//! gathered, in the same shape as the Stage 199D disposition contract: the impure work
//! (resolving capabilities, reading the endpoint incarnation) happens at the call site, the
//! *decision* happens here, and there is **no wildcard arm** — a new decline reason cannot
//! be added without deciding what it means.
//!
//! # NR6 — request eligibility
//!
//! A request is eligible only when all of these hold:
//!
//! 1. the send capability resolves **with `SEND` rights** (the resolver enforces the right
//!    and reports `MissingRight` otherwise);
//! 2. it names an `Endpoint` object;
//! 3. the target endpoint **incarnation is current** — the slot is occupied and its
//!    generation matches the capability's;
//! 4. the endpoint's [`EndpointMode`] is `Buffered`;
//! 5. the message shape is one the direct transaction supports — a payload within
//!    [`crate::kernel::ipccall_direct::IPC_DIRECT_PAYLOAD_MAX`] and a reply-endpoint
//!    receive capability to bind the one-shot Reply object to.
//!
//! **`Synchronous` endpoints decline before any mutation** and fall through to the legacy
//! rendezvous path. The direct transaction claims an endpoint waiter and delivers straight
//! into the receiver's address space; it does not reproduce the scheduling-level rendezvous
//! that `KernelState::ipc_send`/`ipc_recv` enforce for a `Synchronous` endpoint
//! (`ipc_state.rs`), so servicing one off-lock would silently change its semantics.
//!
//! # NR7 — reply eligibility
//!
//! A reply is eligible when the reply capability resolves to a live **one-shot `Reply`
//! object** and that record still names its exact caller and reply-endpoint incarnation.
//! There is deliberately **no `EndpointMode` requirement**: NR7 does not send *to* an
//! endpoint. It consumes a reply authority that the request path already minted, and
//! delivers to a caller that is already committed-blocked on its reply endpoint. The
//! endpoint's queueing discipline never comes into play, so imposing a mode check here
//! would decline correct work for no reason.
//!
//! # Purity
//!
//! Both classifiers take facts by reference and touch no kernel state, no locks and no
//! counters. That is what makes them exhaustively testable — every decline reason can be
//! constructed directly, including the ones that need a live race to occur naturally.

// Consumed by the split-dispatch helpers, which are freestanding-only: a hosted `lib`
// build compiles no route to them, so this whole module reads as dead there. The
// eligibility contract's own tests exercise every item in every build.
#![allow(dead_code)]

use crate::kernel::boot::KernelError;
use crate::kernel::capabilities::CapObject;
use crate::kernel::ipc::EndpointMode;
use crate::kernel::ipccall_direct::IPC_DIRECT_PAYLOAD_MAX;

/// Facts the NR6 call site gathers before the eligibility decision. Gathering these performs
/// only reads; none of it mutates kernel state, so a decline here is free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectRequestFacts {
    /// The `len` argument as the caller supplied it.
    pub(crate) payload_len: usize,
    /// The authoritative current TID was available.
    pub(crate) requester_available: bool,
    /// Resolution of the send capability, with the `SEND` right enforced by the resolver.
    pub(crate) send_cap: Result<CapObject, KernelError>,
    /// The target endpoint's mode, or `None` when the incarnation named by the capability is
    /// no longer current (slot empty, out of range, or generation recycled).
    pub(crate) endpoint_mode: Option<EndpointMode>,
    /// Whether this endpoint is admitted to the off-lock path. On x86_64 the direct
    /// path is the production default, so this is unconditionally true; on other
    /// architectures it is still the oracle endpoint confinement.
    pub(crate) endpoint_admitted: bool,
}

/// Facts the NR7 call site gathers before the eligibility decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectReplyFacts {
    /// The `len` argument as the caller supplied it.
    pub(crate) payload_len: usize,
    /// The authoritative current TID was available.
    pub(crate) requester_available: bool,
    /// Resolution of the one-shot `Reply` object `{index, generation}` the replier's
    /// capability names, with the `SEND` right enforced by the resolver.
    pub(crate) reply_object: Result<(usize, u64), KernelError>,
    /// The reply-endpoint incarnation bound in that record, or `None` when the record is
    /// absent, generation-mismatched, or does not bind an endpoint.
    pub(crate) reply_endpoint: Option<(usize, u64)>,
    /// Whether this reply endpoint is admitted to the off-lock path. See the request twin.
    pub(crate) endpoint_admitted: bool,
    /// Whether the reply carries a **transferred capability** in `SYSCALL_ARG_TRANSFER_CAP`.
    ///
    /// The direct transaction has no notion of capability transfer: it copies the payload and
    /// metadata into the caller's address space and wakes it, with nothing that mints, stashes
    /// or installs a capability. Legacy `ipc_reply` validates the transfer cap and stashes a
    /// transfer handle that the caller's `recv` then installs. So a cap-bearing reply serviced
    /// off-lock would deliver the payload and **silently drop the capability** — the receiver
    /// sees a successful reply with `transferred_cap=0` and no way to tell it was robbed.
    pub(crate) transfer_cap_present: bool,
    /// Whether this reply-record incarnation participates in an **armed terminal-ownership /
    /// reply-timeout race**, read from the authoritative `reply_terminal_ownership` cell and
    /// exact in record index AND generation
    /// (`SharedKernel::reply_record_terminal_arbitrated_split_read`).
    ///
    /// Such a reply is arbitrated: it must reserve the terminal before its caller copy and
    /// commit it after, so that a concurrent timeout claimant provably loses. That lease —
    /// `reserve_reply_win_before_copy` → delivery → `commit_reply_win_after_delivery`, with
    /// `rollback_reply_win` on a retryable copy fault — lives only on the legacy reply path.
    /// The direct transaction neither takes it nor leaves the record in the state the legacy
    /// path expects, so servicing an arbitrated reply off-lock loses the race the caller was
    /// promised: live, the reply reserved, rolled back, and the timeout's deferred path
    /// completed instead.
    pub(crate) terminal_arbitrated: bool,
}

/// The exhaustive NR6 eligibility verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectRequestEligibility {
    /// Serviceable off-lock, on this exact endpoint incarnation.
    Eligible {
        endpoint_index: usize,
        endpoint_generation: u64,
    },
    /// The payload exceeds what the direct snapshot can carry.
    PayloadTooLong,
    /// The authoritative requester identity was unavailable.
    RequesterUnavailable,
    /// The send capability did not resolve, or lacked the `SEND` right.
    SendCapUnresolved(KernelError),
    /// The capability resolved, but not to an `Endpoint`.
    NotAnEndpoint,
    /// The endpoint incarnation named by the capability is no longer current.
    EndpointIncarnationGone,
    /// The endpoint is `Synchronous`: the legacy rendezvous path owns it.
    SynchronousMode,
    /// The endpoint is not admitted to the off-lock path (non-x86 oracle confinement).
    EndpointNotAdmitted,
}

/// The exhaustive NR7 eligibility verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectReplyEligibility {
    /// Serviceable off-lock, against this exact reply-endpoint incarnation.
    Eligible {
        endpoint_index: usize,
        endpoint_generation: u64,
    },
    /// The payload exceeds what the direct snapshot can carry.
    PayloadTooLong,
    /// The authoritative requester identity was unavailable.
    RequesterUnavailable,
    /// The reply capability did not resolve to a live one-shot `Reply` object.
    ReplyCapUnresolved(KernelError),
    /// The record does not bind a reply-endpoint incarnation (absent / stale / not an
    /// endpoint).
    ReplyEndpointGone,
    /// The reply carries a transferred capability, which the direct transaction cannot
    /// deliver. The legacy path owns it; declining here is what stops the capability from
    /// being silently dropped.
    TransferCapUnsupported,
    /// The reply participates in an armed terminal-ownership / reply-timeout race. The
    /// terminal lease that makes the reply provably beat a concurrent timeout lives only on
    /// the legacy path, so this reply is the legacy path's to service.
    TerminalArbitrationUnsupported,
    /// The endpoint is not admitted to the off-lock path (non-x86 oracle confinement).
    EndpointNotAdmitted,
}

impl DirectRequestEligibility {
    /// The endpoint incarnation to service, when eligible.
    pub(crate) fn endpoint(self) -> Option<(usize, u64)> {
        match self {
            Self::Eligible {
                endpoint_index,
                endpoint_generation,
            } => Some((endpoint_index, endpoint_generation)),
            _ => None,
        }
    }

    /// True for the one decline that exists because of the endpoint's *mode*, which the
    /// counters report separately from every other preflight decline.
    pub(crate) fn is_ineligible_mode(self) -> bool {
        matches!(self, Self::SynchronousMode)
    }
}

impl DirectReplyEligibility {
    /// The reply-endpoint incarnation to service, when eligible.
    pub(crate) fn endpoint(self) -> Option<(usize, u64)> {
        match self {
            Self::Eligible {
                endpoint_index,
                endpoint_generation,
            } => Some((endpoint_index, endpoint_generation)),
            _ => None,
        }
    }

    /// True for the one decline that exists because the reply carries a capability the direct
    /// transaction cannot transfer. Reported separately so a live boot can show how much
    /// ordinary reply traffic is cap-bearing — which is the size of the work that
    /// implementing direct capability transfer would unlock.
    pub(crate) fn is_transfer_cap_decline(self) -> bool {
        matches!(self, Self::TransferCapUnsupported)
    }

    /// True for the one decline that exists because a terminal-ownership / reply-timeout race
    /// is arbitrating this reply. Reported separately so a live boot can show exactly how much
    /// of the legacy reply population is arbitrated — which is the size of the work that
    /// porting the terminal lease into the direct transaction would unlock.
    pub(crate) fn is_terminal_arbitration_decline(self) -> bool {
        matches!(self, Self::TerminalArbitrationUnsupported)
    }
}

/// Classify NR6 eligibility. Pure and exhaustive: no wildcard arm on the facts.
///
/// The order is deliberate — cheapest and least privileged first, so a decline costs the
/// least possible work and never depends on a check that could itself mutate:
/// length → requester → capability + rights → object kind → incarnation → mode →
/// confinement.
pub(crate) fn classify_direct_request_eligibility(
    facts: &DirectRequestFacts,
) -> DirectRequestEligibility {
    if facts.payload_len > IPC_DIRECT_PAYLOAD_MAX {
        return DirectRequestEligibility::PayloadTooLong;
    }
    if !facts.requester_available {
        return DirectRequestEligibility::RequesterUnavailable;
    }
    let object = match facts.send_cap {
        Ok(object) => object,
        Err(err) => return DirectRequestEligibility::SendCapUnresolved(err),
    };
    let (index, generation) = match object {
        CapObject::Endpoint { index, generation } => (index, generation),
        CapObject::Kernel
        | CapObject::AddressSpace { .. }
        | CapObject::IovaSpace { .. }
        | CapObject::MemoryObject { .. }
        | CapObject::DmaRegion { .. }
        | CapObject::Notification { .. }
        | CapObject::Reply { .. }
        | CapObject::Irq { .. } => return DirectRequestEligibility::NotAnEndpoint,
    };
    // The incarnation must be CURRENT: a recycled slot is not the endpoint the capability
    // was minted against.
    let mode = match facts.endpoint_mode {
        Some(mode) => mode,
        None => return DirectRequestEligibility::EndpointIncarnationGone,
    };
    match mode {
        // The direct transaction claims a waiter and delivers straight into the receiver's
        // address space; it does not reproduce `Synchronous` rendezvous scheduling.
        EndpointMode::Synchronous => return DirectRequestEligibility::SynchronousMode,
        EndpointMode::Buffered => {}
    }
    if !facts.endpoint_admitted {
        return DirectRequestEligibility::EndpointNotAdmitted;
    }
    DirectRequestEligibility::Eligible {
        endpoint_index: index,
        endpoint_generation: generation,
    }
}

/// Classify NR7 eligibility. Pure and exhaustive.
///
/// Deliberately imposes **no** `EndpointMode` requirement: NR7 consumes a one-shot reply
/// authority and delivers to a caller already committed-blocked on its reply endpoint, so
/// the endpoint's queueing discipline never applies.
///
/// The transfer-cap check runs **before any capability resolution**: it is the cheapest check
/// there is (one already-read frame argument), and a cap-bearing reply must never get far
/// enough to claim an acknowledgement, so putting it early makes that structural rather than
/// incidental.
pub(crate) fn classify_direct_reply_eligibility(
    facts: &DirectReplyFacts,
) -> DirectReplyEligibility {
    if facts.payload_len > IPC_DIRECT_PAYLOAD_MAX {
        return DirectReplyEligibility::PayloadTooLong;
    }
    if !facts.requester_available {
        return DirectReplyEligibility::RequesterUnavailable;
    }
    // The direct transaction copies payload + metadata and wakes the caller. It mints,
    // stashes and installs nothing, so it cannot carry a capability — decline before any
    // mutation and let the legacy path do the transfer.
    if facts.transfer_cap_present {
        return DirectReplyEligibility::TransferCapUnsupported;
    }
    if let Err(err) = facts.reply_object {
        return DirectReplyEligibility::ReplyCapUnresolved(err);
    }
    // The reply record's identity is now known, so the arbitration fact applies. It is checked
    // BEFORE eligibility can be granted, and therefore before the acknowledgement claim, the
    // record reservation or consumption, the payload/meta copy, any waiter mutation or wake,
    // the reverse-link close, and any direct transaction call — every one of which happens
    // only on the `Eligible` arm at the call site.
    if facts.terminal_arbitrated {
        return DirectReplyEligibility::TerminalArbitrationUnsupported;
    }
    let (index, generation) = match facts.reply_endpoint {
        Some(pair) => pair,
        None => return DirectReplyEligibility::ReplyEndpointGone,
    };
    if !facts.endpoint_admitted {
        return DirectReplyEligibility::EndpointNotAdmitted;
    }
    DirectReplyEligibility::Eligible {
        endpoint_index: index,
        endpoint_generation: generation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffered_endpoint_facts() -> DirectRequestFacts {
        DirectRequestFacts {
            payload_len: 8,
            requester_available: true,
            send_cap: Ok(CapObject::Endpoint {
                index: 3,
                generation: 11,
            }),
            endpoint_mode: Some(EndpointMode::Buffered),
            endpoint_admitted: true,
        }
    }

    fn reply_facts() -> DirectReplyFacts {
        DirectReplyFacts {
            payload_len: 8,
            requester_available: true,
            reply_object: Ok((5, 2)),
            reply_endpoint: Some((4, 9)),
            endpoint_admitted: true,
            transfer_cap_present: false,
            terminal_arbitrated: false,
        }
    }

    #[test]
    fn buffered_endpoint_with_send_rights_is_eligible() {
        assert_eq!(
            classify_direct_request_eligibility(&buffered_endpoint_facts()),
            DirectRequestEligibility::Eligible {
                endpoint_index: 3,
                endpoint_generation: 11,
            }
        );
    }

    /// The headline rule: a `Synchronous` endpoint declines, and it declines for that
    /// reason specifically — not folded into a generic preflight decline.
    #[test]
    fn synchronous_endpoint_declines_as_a_mode_decline() {
        let mut facts = buffered_endpoint_facts();
        facts.endpoint_mode = Some(EndpointMode::Synchronous);
        let verdict = classify_direct_request_eligibility(&facts);
        assert_eq!(verdict, DirectRequestEligibility::SynchronousMode);
        assert!(verdict.is_ineligible_mode());
        assert_eq!(verdict.endpoint(), None, "a decline services nothing");
        // And it is the ONLY mode decline.
        for other in [
            DirectRequestEligibility::PayloadTooLong,
            DirectRequestEligibility::RequesterUnavailable,
            DirectRequestEligibility::SendCapUnresolved(KernelError::MissingRight),
            DirectRequestEligibility::NotAnEndpoint,
            DirectRequestEligibility::EndpointIncarnationGone,
            DirectRequestEligibility::EndpointNotAdmitted,
        ] {
            assert!(!other.is_ineligible_mode(), "{other:?}");
        }
    }

    /// The mode check runs BEFORE the confinement check, so a `Synchronous` endpoint is
    /// reported as a mode decline whether or not it is the oracle's endpoint. This keeps the
    /// counter meaningful once the confinement is eventually removed.
    #[test]
    fn synchronous_mode_is_reported_even_outside_the_confinement() {
        let mut facts = buffered_endpoint_facts();
        facts.endpoint_mode = Some(EndpointMode::Synchronous);
        facts.endpoint_admitted = false;
        assert_eq!(
            classify_direct_request_eligibility(&facts),
            DirectRequestEligibility::SynchronousMode
        );
    }

    #[test]
    fn missing_send_right_declines_with_the_resolver_error() {
        let mut facts = buffered_endpoint_facts();
        facts.send_cap = Err(KernelError::MissingRight);
        assert_eq!(
            classify_direct_request_eligibility(&facts),
            DirectRequestEligibility::SendCapUnresolved(KernelError::MissingRight)
        );
        facts.send_cap = Err(KernelError::InvalidCapability);
        assert_eq!(
            classify_direct_request_eligibility(&facts),
            DirectRequestEligibility::SendCapUnresolved(KernelError::InvalidCapability)
        );
    }

    #[test]
    fn stale_endpoint_incarnation_declines() {
        let mut facts = buffered_endpoint_facts();
        facts.endpoint_mode = None;
        assert_eq!(
            classify_direct_request_eligibility(&facts),
            DirectRequestEligibility::EndpointIncarnationGone
        );
    }

    #[test]
    fn a_non_endpoint_object_declines() {
        let mut facts = buffered_endpoint_facts();
        facts.send_cap = Ok(CapObject::Reply {
            index: 1,
            generation: 1,
        });
        assert_eq!(
            classify_direct_request_eligibility(&facts),
            DirectRequestEligibility::NotAnEndpoint
        );
    }

    #[test]
    fn oversized_payload_declines_before_anything_else() {
        let mut facts = buffered_endpoint_facts();
        facts.payload_len = IPC_DIRECT_PAYLOAD_MAX + 1;
        // Even with every other fact broken, length is reported first — it is the cheapest
        // check and needs no capability resolution at all.
        facts.send_cap = Err(KernelError::InvalidCapability);
        facts.endpoint_mode = None;
        facts.requester_available = false;
        assert_eq!(
            classify_direct_request_eligibility(&facts),
            DirectRequestEligibility::PayloadTooLong
        );
        // The exact maximum is still eligible.
        let mut ok = buffered_endpoint_facts();
        ok.payload_len = IPC_DIRECT_PAYLOAD_MAX;
        assert!(
            classify_direct_request_eligibility(&ok)
                .endpoint()
                .is_some()
        );
    }

    #[test]
    fn unavailable_requester_declines() {
        let mut facts = buffered_endpoint_facts();
        facts.requester_available = false;
        assert_eq!(
            classify_direct_request_eligibility(&facts),
            DirectRequestEligibility::RequesterUnavailable
        );
    }

    #[test]
    fn confinement_is_unchanged_and_still_declines() {
        let mut facts = buffered_endpoint_facts();
        facts.endpoint_admitted = false;
        assert_eq!(
            classify_direct_request_eligibility(&facts),
            DirectRequestEligibility::EndpointNotAdmitted
        );
    }

    // ── NR7 ────────────────────────────────────────────────────────────────────────────

    #[test]
    fn a_live_reply_object_is_eligible() {
        assert_eq!(
            classify_direct_reply_eligibility(&reply_facts()),
            DirectReplyEligibility::Eligible {
                endpoint_index: 4,
                endpoint_generation: 9,
            }
        );
    }

    /// NR7 imposes NO mode requirement: it consumes a one-shot reply authority and delivers
    /// to an already-blocked caller, so the endpoint's queueing discipline never applies.
    /// This is asserted structurally — the facts carry no mode at all — and by inspection of
    /// the classifier's source.
    #[test]
    fn reply_eligibility_has_no_endpoint_mode_requirement() {
        let src = include_str!("direct_eligibility.rs");
        let body = src
            .split("pub(crate) fn classify_direct_reply_eligibility(")
            .nth(1)
            .expect("classifier present")
            .split("\n}\n")
            .next()
            .expect("body bounded");
        assert!(
            !body.contains("EndpointMode"),
            "NR7 must not invent an EndpointMode requirement"
        );
        // A reply whose bound endpoint happens to be Synchronous is still eligible: the
        // facts type has no place to express a mode, by design.
        assert!(
            classify_direct_reply_eligibility(&reply_facts())
                .endpoint()
                .is_some()
        );
    }

    #[test]
    fn reply_cap_resolution_failure_declines() {
        let mut facts = reply_facts();
        facts.reply_object = Err(KernelError::WrongObject);
        assert_eq!(
            classify_direct_reply_eligibility(&facts),
            DirectReplyEligibility::ReplyCapUnresolved(KernelError::WrongObject)
        );
        facts.reply_object = Err(KernelError::StaleCapability);
        assert_eq!(
            classify_direct_reply_eligibility(&facts),
            DirectReplyEligibility::ReplyCapUnresolved(KernelError::StaleCapability)
        );
    }

    #[test]
    fn missing_reply_endpoint_binding_declines() {
        let mut facts = reply_facts();
        facts.reply_endpoint = None;
        assert_eq!(
            classify_direct_reply_eligibility(&facts),
            DirectReplyEligibility::ReplyEndpointGone
        );
    }

    #[test]
    fn reply_confinement_is_unchanged_and_still_declines() {
        let mut facts = reply_facts();
        facts.endpoint_admitted = false;
        assert_eq!(
            classify_direct_reply_eligibility(&facts),
            DirectReplyEligibility::EndpointNotAdmitted
        );
    }

    #[test]
    fn reply_oversized_payload_and_unavailable_requester_decline() {
        let mut facts = reply_facts();
        facts.payload_len = IPC_DIRECT_PAYLOAD_MAX + 1;
        assert_eq!(
            classify_direct_reply_eligibility(&facts),
            DirectReplyEligibility::PayloadTooLong
        );
        let mut facts = reply_facts();
        facts.requester_available = false;
        assert_eq!(
            classify_direct_reply_eligibility(&facts),
            DirectReplyEligibility::RequesterUnavailable
        );
    }

    // ── NR7 transfer-capability safety ─────────────────────────────────────────────────

    /// **The headline rule.** A reply carrying a transferred capability is ineligible, and it
    /// declines for that reason specifically — not folded into a generic decline — because the
    /// direct transaction has no way to deliver the capability and would otherwise deliver the
    /// payload and drop it silently.
    #[test]
    fn a_cap_bearing_reply_is_ineligible() {
        let mut facts = reply_facts();
        facts.transfer_cap_present = true;
        let verdict = classify_direct_reply_eligibility(&facts);
        assert_eq!(verdict, DirectReplyEligibility::TransferCapUnsupported);
        assert!(verdict.is_transfer_cap_decline());
        assert_eq!(
            verdict.endpoint(),
            None,
            "a decline services nothing: no endpoint to claim an acknowledgement for"
        );
        // And it is the ONLY transfer-cap decline.
        for other in [
            DirectReplyEligibility::PayloadTooLong,
            DirectReplyEligibility::RequesterUnavailable,
            DirectReplyEligibility::ReplyCapUnresolved(KernelError::WrongObject),
            DirectReplyEligibility::ReplyEndpointGone,
            DirectReplyEligibility::EndpointNotAdmitted,
            DirectReplyEligibility::Eligible {
                endpoint_index: 1,
                endpoint_generation: 1,
            },
        ] {
            assert!(!other.is_transfer_cap_decline(), "{other:?}");
        }
    }

    /// A reply with no transferred capability stays direct-eligible — the check narrows the
    /// direct path, it does not close it.
    #[test]
    fn a_reply_without_a_transfer_cap_remains_eligible() {
        let facts = reply_facts();
        assert!(!facts.transfer_cap_present);
        assert_eq!(
            classify_direct_reply_eligibility(&facts),
            DirectReplyEligibility::Eligible {
                endpoint_index: 4,
                endpoint_generation: 9,
            }
        );
    }

    /// The transfer-cap decline wins over every *later* check, so a cap-bearing reply can
    /// never reach capability resolution, an endpoint incarnation, or an acknowledgement —
    /// no matter what else is true about it. This is what makes "cannot enter the direct
    /// transaction" a property of the ordering rather than of the call site's care.
    #[test]
    fn a_cap_bearing_reply_declines_before_any_capability_resolution() {
        let mut facts = reply_facts();
        facts.transfer_cap_present = true;
        // Perfectly valid reply object, live endpoint, admitted: still declined.
        assert_eq!(
            classify_direct_reply_eligibility(&facts),
            DirectReplyEligibility::TransferCapUnsupported
        );
        // Broken reply object: the transfer-cap decline is still what is reported, so the
        // classifier never even inspects the capability.
        facts.reply_object = Err(KernelError::InvalidCapability);
        facts.reply_endpoint = None;
        facts.endpoint_admitted = false;
        assert_eq!(
            classify_direct_reply_eligibility(&facts),
            DirectReplyEligibility::TransferCapUnsupported
        );
        // Only the two checks that need no capability at all outrank it.
        let mut too_long = reply_facts();
        too_long.transfer_cap_present = true;
        too_long.payload_len = IPC_DIRECT_PAYLOAD_MAX + 1;
        assert_eq!(
            classify_direct_reply_eligibility(&too_long),
            DirectReplyEligibility::PayloadTooLong
        );
        let mut no_requester = reply_facts();
        no_requester.transfer_cap_present = true;
        no_requester.requester_available = false;
        assert_eq!(
            classify_direct_reply_eligibility(&no_requester),
            DirectReplyEligibility::RequesterUnavailable
        );
    }

    /// NR6 has no transfer-cap fact, and must not grow one: `IpcCall` repurposes the
    /// `SYSCALL_ARG_TRANSFER_CAP` slot for the reply-endpoint receive capability, which the
    /// direct request path already reads the same way the legacy handler does.
    #[test]
    fn request_eligibility_has_no_transfer_cap_concept() {
        let src = include_str!("direct_eligibility.rs");
        let facts = src
            .split("pub(crate) struct DirectRequestFacts {")
            .nth(1)
            .expect("facts present")
            .split("\n}\n")
            .next()
            .expect("body bounded");
        assert!(
            !facts.contains("transfer_cap"),
            "NR6 has no transfer capability in flight"
        );
        let body = src
            .split("pub(crate) fn classify_direct_request_eligibility(")
            .nth(1)
            .expect("classifier present")
            .split("\n}\n")
            .next()
            .expect("body bounded");
        assert!(!body.contains("transfer_cap"));
    }

    /// The transfer-cap check is a decline, never a failure: it yields no endpoint, so the
    /// call site's `verdict.endpoint()` guard is what makes it mutation-free. Pinned here so
    /// nobody can turn it into an error return that would change the syscall's ABI.
    #[test]
    fn the_transfer_cap_decline_carries_no_error_and_no_endpoint() {
        let verdict = DirectReplyEligibility::TransferCapUnsupported;
        assert_eq!(verdict.endpoint(), None);
        // It is not an error-carrying variant — the legacy path returns the real result.
        assert!(!matches!(
            verdict,
            DirectReplyEligibility::ReplyCapUnresolved(_)
        ));
    }

    // ── NR7 terminal-arbitration safety ────────────────────────────────────────────────

    /// **The headline rule.** A reply whose record is arbitrated by an armed
    /// terminal-ownership / reply-timeout race is ineligible, and declines for that reason
    /// specifically. The terminal lease that makes such a reply provably beat a concurrent
    /// timeout lives only on the legacy path.
    #[test]
    fn a_terminal_arbitrated_reply_is_ineligible() {
        let mut facts = reply_facts();
        facts.terminal_arbitrated = true;
        let verdict = classify_direct_reply_eligibility(&facts);
        assert_eq!(
            verdict,
            DirectReplyEligibility::TerminalArbitrationUnsupported
        );
        assert!(verdict.is_terminal_arbitration_decline());
        assert_eq!(
            verdict.endpoint(),
            None,
            "a decline services nothing: no endpoint to claim an acknowledgement for"
        );
        // It is the ONLY terminal-arbitration decline.
        for other in [
            DirectReplyEligibility::PayloadTooLong,
            DirectReplyEligibility::RequesterUnavailable,
            DirectReplyEligibility::ReplyCapUnresolved(KernelError::WrongObject),
            DirectReplyEligibility::ReplyEndpointGone,
            DirectReplyEligibility::TransferCapUnsupported,
            DirectReplyEligibility::EndpointNotAdmitted,
            DirectReplyEligibility::Eligible {
                endpoint_index: 1,
                endpoint_generation: 1,
            },
        ] {
            assert!(!other.is_terminal_arbitration_decline(), "{other:?}");
        }
    }

    /// An ORDINARY unarmed reply stays direct-eligible — the check narrows the direct path to
    /// exactly the arbitrated population, it does not close it.
    #[test]
    fn an_unarmed_reply_remains_eligible() {
        let facts = reply_facts();
        assert!(!facts.terminal_arbitrated);
        assert_eq!(
            classify_direct_reply_eligibility(&facts),
            DirectReplyEligibility::Eligible {
                endpoint_index: 4,
                endpoint_generation: 9,
            }
        );
    }

    /// The arbitration decline wins over every *later* check, so an arbitrated reply can never
    /// reach an endpoint incarnation, an acknowledgement claim or the transaction — the
    /// property is one of ordering, not of the call site's care.
    #[test]
    fn an_arbitrated_reply_declines_before_the_endpoint_is_even_resolved() {
        let mut facts = reply_facts();
        facts.terminal_arbitrated = true;
        assert_eq!(
            classify_direct_reply_eligibility(&facts),
            DirectReplyEligibility::TerminalArbitrationUnsupported
        );
        // Even with the endpoint gone and the endpoint unadmitted, arbitration is what is
        // reported — so the classifier never inspects them for an arbitrated reply.
        facts.reply_endpoint = None;
        facts.endpoint_admitted = false;
        assert_eq!(
            classify_direct_reply_eligibility(&facts),
            DirectReplyEligibility::TerminalArbitrationUnsupported
        );
        // Only the checks that need no record identity at all outrank it: an unresolvable
        // reply object has no record for the arbitration fact to be about.
        let mut unresolved = reply_facts();
        unresolved.terminal_arbitrated = true;
        unresolved.reply_object = Err(KernelError::InvalidCapability);
        assert_eq!(
            classify_direct_reply_eligibility(&unresolved),
            DirectReplyEligibility::ReplyCapUnresolved(KernelError::InvalidCapability)
        );
        let mut too_long = reply_facts();
        too_long.terminal_arbitrated = true;
        too_long.payload_len = IPC_DIRECT_PAYLOAD_MAX + 1;
        assert_eq!(
            classify_direct_reply_eligibility(&too_long),
            DirectReplyEligibility::PayloadTooLong
        );
    }

    /// NR6 has no arbitration fact and must not grow one: a request does not resolve a reply
    /// record, so there is no terminal cell for it to race against.
    #[test]
    fn request_eligibility_has_no_terminal_arbitration_concept() {
        let src = include_str!("direct_eligibility.rs");
        let facts = src
            .split("pub(crate) struct DirectRequestFacts {")
            .nth(1)
            .expect("facts present")
            .split("\n}\n")
            .next()
            .expect("body bounded");
        assert!(!facts.contains("terminal_arbitrated"));
        let body = src
            .split("pub(crate) fn classify_direct_request_eligibility(")
            .nth(1)
            .expect("classifier present")
            .split("\n}\n")
            .next()
            .expect("body bounded");
        assert!(!body.contains("terminal_arbitrated"));
    }

    /// The arbitration check is a DECLINE, never a failure: it yields no endpoint, so the call
    /// site's `verdict.endpoint()` guard is what makes it mutation-free, and the legacy path
    /// returns the real result.
    #[test]
    fn the_arbitration_decline_carries_no_error_and_no_endpoint() {
        let verdict = DirectReplyEligibility::TerminalArbitrationUnsupported;
        assert_eq!(verdict.endpoint(), None);
        assert!(!matches!(
            verdict,
            DirectReplyEligibility::ReplyCapUnresolved(_)
        ));
    }

    /// Exhaustiveness: neither classifier has a wildcard arm on the facts it decides over.
    #[test]
    fn classifiers_are_exhaustive_with_no_wildcard_on_the_decision() {
        let src = include_str!("direct_eligibility.rs");
        for classifier in [
            "pub(crate) fn classify_direct_request_eligibility(",
            "pub(crate) fn classify_direct_reply_eligibility(",
        ] {
            let body = src
                .split(classifier)
                .nth(1)
                .expect("classifier present")
                .split("\n}\n")
                .next()
                .expect("body bounded");
            assert!(
                !body.contains("_ =>"),
                "{classifier}: no wildcard arm on the eligibility decision"
            );
            // Purity: no kernel access, no locks, no counters.
            for forbidden in ["self.", "with_ipc", "_split_read", "note_", "fetch_add"] {
                assert!(
                    !body.contains(forbidden),
                    "{classifier}: must stay pure ({forbidden})"
                );
            }
        }
    }
}
