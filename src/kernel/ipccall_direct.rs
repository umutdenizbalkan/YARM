// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199A2B1 — architecture-neutral foundations for the off-lock direct
//! `IpcCall` (NR 6) request and `IpcReply` (NR 7) reply paths on x86_64.
//!
//! This module holds the **bounded, owned, by-value** building blocks that the
//! (future) x86 split-dispatch wiring publishes and consumes — with **no**
//! `&mut KernelState`, no borrowed subsystem references, and no raw userspace
//! pointer. Two things live here:
//!
//! 1. [`IpcCallDirectSnapshot`] / [`IpcReplyDirectSnapshot`] — the owned pre-lock
//!    snapshots captured at trap entry. The source userspace payload is copied
//!    into the snapshot's owned `[u8; Message::MAX_PAYLOAD]` buffer *before* any
//!    broad/domain lock is taken; nothing downstream ever dereferences a raw
//!    userspace pointer. Over-length input yields `None` (no snapshot), so a
//!    length rejection mutates no IPC / capability / waiter / reply-record /
//!    scheduler state.
//!
//! 2. [`ReplyReservation`] — the **reversible one-shot** reply-record state
//!    machine (`Available → Reserved → Consumed`). It lets the NR 7 reply path
//!    reserve the sole reply authority, copy the reply to the caller *outside all
//!    locks*, and only THEN consume — rolling back `Reserved → Available` on a
//!    caller-side copy fault so the caller stays blocked with its reply authority
//!    intact and zero wake. A duplicate/aliased invocation while `Reserved` or
//!    `Consumed` fails and copies/wakes nothing.
//!
//! These types are exercised by their own unit tests here; the live x86
//! split-dispatch integration + userspace round-trip oracle + QEMU seal are the
//! remaining deep work (tracked in the stage report), gated default-off behind
//! the `x86-ipccall-direct-oracle` feature.

use super::boot::ReceiverWaiterIdentity;
use super::capabilities::CapId;
use super::ipc::Message;

/// Bounded payload capacity of a direct IPC snapshot — held in lockstep with the
/// kernel IPC message payload bound (the task specifies `[u8; 128]`).
pub(crate) const IPC_DIRECT_PAYLOAD_MAX: usize = Message::MAX_PAYLOAD;
const _: () = assert!(IPC_DIRECT_PAYLOAD_MAX == 128);

/// Owned pre-lock snapshot of a direct `IpcCall` (NR 6) request.
///
/// Captured at x86 trap entry with **no lock held**: syscall args snapshotted,
/// length validated, source userspace payload copied into `payload`. No raw
/// userspace pointer survives this phase — `payload[..payload_len]` is the sole
/// authoritative request bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IpcCallDirectSnapshot {
    /// Generation-bearing caller (blocked requester) identity.
    pub(crate) caller: ReceiverWaiterIdentity,
    /// Caller cnode CapId of the SEND endpoint the request is sent on.
    pub(crate) send_endpoint_cap: CapId,
    /// Caller cnode CapId of the RECEIVE endpoint the caller will block on for the
    /// reply.
    pub(crate) reply_endpoint_cap: CapId,
    /// Owned request payload bytes (bounded).
    pub(crate) payload: [u8; IPC_DIRECT_PAYLOAD_MAX],
    /// Valid prefix length of `payload`.
    pub(crate) payload_len: usize,
}

impl IpcCallDirectSnapshot {
    /// Build the owned snapshot, copying `src` into the owned buffer. Returns `None`
    /// (no snapshot, no state mutated) when `src` exceeds the bounded capacity — the
    /// caller treats that exactly like the canonical `InvalidArgs` length rejection.
    pub(crate) fn build(
        caller: ReceiverWaiterIdentity,
        send_endpoint_cap: CapId,
        reply_endpoint_cap: CapId,
        src: &[u8],
    ) -> Option<Self> {
        if src.len() > IPC_DIRECT_PAYLOAD_MAX {
            return None;
        }
        let mut payload = [0u8; IPC_DIRECT_PAYLOAD_MAX];
        payload[..src.len()].copy_from_slice(src);
        Some(Self {
            caller,
            send_endpoint_cap,
            reply_endpoint_cap,
            payload,
            payload_len: src.len(),
        })
    }

    /// The authoritative owned request bytes.
    pub(crate) fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_len]
    }
}

/// Owned pre-lock snapshot of a direct `IpcReply` (NR 7) reply.
///
/// Captured at x86 trap entry with **no lock held**. The source replier payload is
/// copied into `payload` before any reservation/claim — `source-copy-before-claim`
/// is structural, not incidental.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IpcReplyDirectSnapshot {
    /// Generation-bearing replier (responder) identity.
    pub(crate) replier: ReceiverWaiterIdentity,
    /// Replier cnode CapId of the one-shot Reply cap.
    pub(crate) reply_cap: CapId,
    /// Owned reply payload bytes (bounded).
    pub(crate) payload: [u8; IPC_DIRECT_PAYLOAD_MAX],
    /// Valid prefix length of `payload`.
    pub(crate) payload_len: usize,
}

impl IpcReplyDirectSnapshot {
    /// Build the owned reply snapshot, copying `src` into the owned buffer. `None`
    /// on over-length (no snapshot, nothing mutated).
    pub(crate) fn build(
        replier: ReceiverWaiterIdentity,
        reply_cap: CapId,
        src: &[u8],
    ) -> Option<Self> {
        if src.len() > IPC_DIRECT_PAYLOAD_MAX {
            return None;
        }
        let mut payload = [0u8; IPC_DIRECT_PAYLOAD_MAX];
        payload[..src.len()].copy_from_slice(src);
        Some(Self {
            replier,
            reply_cap,
            payload,
            payload_len: src.len(),
        })
    }

    /// The authoritative owned reply bytes.
    pub(crate) fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_len]
    }
}

/// Error outcomes of a [`ReplyReservation`] transition. Every one is a
/// fail-closed rejection that mutates no state and copies/wakes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplyReservationError {
    /// `reserve` attempted while already `Reserved` or `Consumed` — a
    /// duplicate/aliased invocation. It must not copy or wake.
    NotAvailable,
    /// `release`/`consume` attempted while not `Reserved`.
    NotReserved,
    /// `release`/`consume` presented the wrong reservation generation (a stale or
    /// aliased holder).
    GenerationMismatch,
    /// `consume` presented a different replier identity than the reservation bound.
    ReplierMismatch,
}

/// The reversible one-shot reply-record reservation state machine.
///
/// `Available → Reserved { reservation_generation, replier } → Consumed`.
///
/// The reservation is taken AFTER the replier payload is copied into an owned
/// snapshot and the bound replier + exact caller reply-endpoint waiter are
/// validated, but BEFORE the reply is copied to the caller. On a caller-side copy
/// fault the holder calls [`release`](Self::release) (`Reserved → Available`),
/// leaving the reply authority usable and the caller blocked with zero wake. Only
/// after the caller copy AND a final revalidation succeed does the holder call
/// [`consume`](Self::consume) (`Reserved → Consumed`), after which reply-cap
/// aliases are revoked and the caller is woken exactly once. A second consume — or
/// any transition from `Consumed` — fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplyReservation {
    /// The reply authority is live and unclaimed.
    Available,
    /// The reply authority is held for exactly one in-flight reply attempt.
    Reserved {
        reservation_generation: u64,
        replier: ReceiverWaiterIdentity,
    },
    /// The reply authority has been irrevocably consumed (one-shot).
    Consumed,
}

impl ReplyReservation {
    pub(crate) const fn new() -> Self {
        Self::Available
    }

    pub(crate) const fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    pub(crate) const fn is_reserved(&self) -> bool {
        matches!(self, Self::Reserved { .. })
    }

    pub(crate) const fn is_consumed(&self) -> bool {
        matches!(self, Self::Consumed)
    }

    /// `Available → Reserved`. A duplicate/aliased invocation (state already
    /// `Reserved` or `Consumed`) is rejected `NotAvailable` — no copy, no wake.
    pub(crate) fn reserve(
        &mut self,
        reservation_generation: u64,
        replier: ReceiverWaiterIdentity,
    ) -> Result<(), ReplyReservationError> {
        match self {
            Self::Available => {
                *self = Self::Reserved {
                    reservation_generation,
                    replier,
                };
                Ok(())
            }
            _ => Err(ReplyReservationError::NotAvailable),
        }
    }

    /// `Reserved → Available` — caller-side copy-fault rollback. The reply
    /// authority stays usable; the caller stays blocked; zero wake. Requires the
    /// exact reservation generation held by the in-flight attempt.
    pub(crate) fn release(
        &mut self,
        reservation_generation: u64,
    ) -> Result<(), ReplyReservationError> {
        match self {
            Self::Reserved {
                reservation_generation: g,
                ..
            } if *g == reservation_generation => {
                *self = Self::Available;
                Ok(())
            }
            Self::Reserved { .. } => Err(ReplyReservationError::GenerationMismatch),
            _ => Err(ReplyReservationError::NotReserved),
        }
    }

    /// `Reserved → Consumed` — final one-shot commit after the caller copy and
    /// revalidation succeed. Requires the matching reservation generation AND the
    /// bound replier identity. A second consume (or consume from `Available` /
    /// `Consumed`) fails.
    pub(crate) fn consume(
        &mut self,
        reservation_generation: u64,
        replier: ReceiverWaiterIdentity,
    ) -> Result<(), ReplyReservationError> {
        match self {
            Self::Reserved {
                reservation_generation: g,
                replier: bound,
            } => {
                if *g != reservation_generation {
                    return Err(ReplyReservationError::GenerationMismatch);
                }
                if *bound != replier {
                    return Err(ReplyReservationError::ReplierMismatch);
                }
                *self = Self::Consumed;
                Ok(())
            }
            _ => Err(ReplyReservationError::NotReserved),
        }
    }
}
/// Authoritative committed blocked-server acknowledgement for the direct NR6
/// request transaction (Stage 199A2B2, Part 2).
///
/// It is a bounded, owned, by-value token representing that a request server is
/// **committed-blocked** waiting to receive on the request endpoint. It is produced
/// ONLY after the complete waiter slot AND the RecvV2 blocked-receive state exist,
/// and it carries everything the off-lock transaction needs to deliver + finalize
/// without re-reading userspace: the server's generation-bearing identity, the
/// endpoint index + generation (the exact-waiter claim authority), a flag that the
/// blocked receive is RecvV2-committed, and the server-side payload/metadata
/// destinations.
///
/// The direct transaction REQUIRES and CONSUMES the exact acknowledgement: with no
/// acknowledgement it returns canonical `WouldBlock` **before** reserving the reply
/// record / minting any cap / mutating any waiter, and it never falls through to
/// queued-call behavior. The acknowledgement is consumed only after the split-work
/// item is installed, and restored on every pre-publication failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockedServerAck {
    /// Generation-bearing server (blocked receiver) identity.
    pub(crate) server: ReceiverWaiterIdentity,
    /// Request endpoint slot index whose waiter is the server.
    pub(crate) endpoint_index: usize,
    /// Request endpoint generation at acknowledgement time (exact-waiter authority).
    pub(crate) endpoint_generation: u64,
    /// The server's blocked receive is a committed RecvV2 operation.
    pub(crate) recv_v2_committed: bool,
    /// Server userspace destination for the request payload.
    pub(crate) payload_user_ptr: usize,
    /// Server userspace payload destination length bound.
    pub(crate) payload_user_len: usize,
    /// Server userspace destination for the recv-v2 metadata.
    pub(crate) meta_user_ptr: usize,
    /// Server userspace metadata destination length bound.
    pub(crate) meta_user_len: usize,
}

impl BlockedServerAck {
    /// True when the acknowledgement is well-formed for a direct transaction: a
    /// committed RecvV2 blocked receive with a payload destination. A malformed or
    /// non-committed acknowledgement is treated as "no acknowledgement" → canonical
    /// `WouldBlock`, never queued fallback.
    pub(crate) const fn is_committed(&self) -> bool {
        self.recv_v2_committed && self.payload_user_ptr != 0
    }

    /// The exact endpoint-waiter claim coordinates `(index, generation, identity)`
    /// the transaction must present to `sr_claim_endpoint_waiter_split`.
    pub(crate) const fn waiter_claim_key(&self) -> (usize, u64, ReceiverWaiterIdentity) {
        (self.endpoint_index, self.endpoint_generation, self.server)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::vm::Asid;

    fn ident(tid: u64, asid: u16) -> ReceiverWaiterIdentity {
        ReceiverWaiterIdentity::new(ThreadId(tid), Asid(asid))
    }

    // ── Snapshots ────────────────────────────────────────────────────────────

    #[test]
    fn call_snapshot_owns_bytes_and_validates_length() {
        let snap =
            IpcCallDirectSnapshot::build(ident(1, 7), CapId(10), CapId(11), b"request-bytes")
                .expect("in-bounds payload builds");
        assert_eq!(snap.payload(), b"request-bytes");
        assert_eq!(snap.payload_len, b"request-bytes".len());
        assert_eq!(snap.send_endpoint_cap, CapId(10));
        assert_eq!(snap.reply_endpoint_cap, CapId(11));
        // Over-length input yields NO snapshot (nothing to mutate downstream).
        let too_big = [0u8; IPC_DIRECT_PAYLOAD_MAX + 1];
        assert!(
            IpcCallDirectSnapshot::build(ident(1, 7), CapId(10), CapId(11), &too_big).is_none()
        );
        // Exactly at the bound is accepted.
        let exact = [0u8; IPC_DIRECT_PAYLOAD_MAX];
        assert!(IpcCallDirectSnapshot::build(ident(1, 7), CapId(10), CapId(11), &exact).is_some());
    }

    #[test]
    fn reply_snapshot_owns_bytes_and_validates_length() {
        let snap = IpcReplyDirectSnapshot::build(ident(2, 9), CapId(5), b"reply")
            .expect("in-bounds payload builds");
        assert_eq!(snap.payload(), b"reply");
        assert_eq!(snap.reply_cap, CapId(5));
        let too_big = [0u8; IPC_DIRECT_PAYLOAD_MAX + 1];
        assert!(IpcReplyDirectSnapshot::build(ident(2, 9), CapId(5), &too_big).is_none());
    }

    // ── Reversible one-shot reservation FSM ───────────────────────────────────

    #[test]
    fn reservation_available_to_reserved_to_consumed() {
        let mut r = ReplyReservation::new();
        assert!(r.is_available());
        r.reserve(1, ident(2, 9)).expect("reserve from Available");
        assert!(r.is_reserved());
        r.consume(1, ident(2, 9)).expect("consume from Reserved");
        assert!(r.is_consumed());
    }

    #[test]
    fn caller_copy_fault_rolls_reserved_back_to_available() {
        let mut r = ReplyReservation::new();
        r.reserve(3, ident(2, 9)).expect("reserve");
        // Simulate a caller-side copy fault: roll back. Authority remains usable.
        r.release(3).expect("release rolls back");
        assert!(
            r.is_available(),
            "reply authority is usable again after rollback"
        );
        // And a fresh reservation can proceed and consume.
        r.reserve(4, ident(2, 9))
            .expect("re-reserve after rollback");
        r.consume(4, ident(2, 9)).expect("consume after rollback");
        assert!(r.is_consumed());
    }

    #[test]
    fn duplicate_reserve_while_reserved_fails_no_progress() {
        let mut r = ReplyReservation::new();
        r.reserve(1, ident(2, 9)).expect("reserve");
        assert_eq!(
            r.reserve(2, ident(3, 4)),
            Err(ReplyReservationError::NotAvailable),
            "a duplicate/aliased reserve while Reserved must fail"
        );
        // State unchanged — still reserved by the first holder.
        assert_eq!(
            r,
            ReplyReservation::Reserved {
                reservation_generation: 1,
                replier: ident(2, 9)
            }
        );
    }

    #[test]
    fn first_consume_is_one_shot_second_fails() {
        let mut r = ReplyReservation::new();
        r.reserve(1, ident(2, 9)).expect("reserve");
        r.consume(1, ident(2, 9)).expect("first consume");
        assert_eq!(
            r.consume(1, ident(2, 9)),
            Err(ReplyReservationError::NotReserved),
            "a second consume must fail (one-shot)"
        );
        // A reserve from Consumed also fails.
        assert_eq!(
            r.reserve(2, ident(2, 9)),
            Err(ReplyReservationError::NotAvailable)
        );
    }

    #[test]
    fn consume_requires_matching_generation_and_replier() {
        let mut r = ReplyReservation::new();
        r.reserve(7, ident(2, 9)).expect("reserve");
        assert_eq!(
            r.consume(8, ident(2, 9)),
            Err(ReplyReservationError::GenerationMismatch)
        );
        assert_eq!(
            r.consume(7, ident(2, 10)),
            Err(ReplyReservationError::ReplierMismatch),
            "a replier at the same numeric TID but different ASID cannot consume"
        );
        // Still reserved — no partial mutation.
        assert!(r.is_reserved());
        r.consume(7, ident(2, 9)).expect("correct holder consumes");
    }

    #[test]
    fn release_requires_matching_generation_and_only_from_reserved() {
        let mut r = ReplyReservation::new();
        assert_eq!(r.release(1), Err(ReplyReservationError::NotReserved));
        r.reserve(5, ident(2, 9)).expect("reserve");
        assert_eq!(r.release(6), Err(ReplyReservationError::GenerationMismatch));
        assert!(r.is_reserved());
        r.release(5).expect("correct generation releases");
        assert!(r.is_available());
    }

    // ── Committed blocked-server acknowledgement ───────────────────────────────

    #[test]
    fn blocked_server_ack_committed_predicate_and_claim_key() {
        let ack = BlockedServerAck {
            server: ident(5, 3),
            endpoint_index: 2,
            endpoint_generation: 9,
            recv_v2_committed: true,
            payload_user_ptr: 0x4000_0000,
            payload_user_len: 128,
            meta_user_ptr: 0x4000_1000,
            meta_user_len: 40,
        };
        assert!(ack.is_committed());
        assert_eq!(ack.waiter_claim_key(), (2, 9, ident(5, 3)));
        // A non-committed or destination-less ack is treated as "no acknowledgement".
        let not_committed = BlockedServerAck {
            recv_v2_committed: false,
            ..ack
        };
        assert!(!not_committed.is_committed());
        let no_dest = BlockedServerAck {
            payload_user_ptr: 0,
            ..ack
        };
        assert!(!no_dest.is_committed());
    }
}
