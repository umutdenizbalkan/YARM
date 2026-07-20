// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199A2B2D — the composed OFF-LOCK NR6 `IpcCallDirectRequest` transaction.
//!
//! [`SharedKernel::ipc_call_direct_request_txn`] delivers a direct request to an
//! exact committed-blocked recv-v2 server ENTIRELY off the broad `&mut KernelState`
//! lock, composing the accepted split seams: the owned reply-record reservation
//! (`reserve/bind/commit/cancel_direct_reply_record_split`), the rank-4 provisional
//! reply-cap mint (`sr_mint_split` / `sr_revoke_split`), the off-lock user copy
//! (`copy_slice_to_user_asid_split_write`), and the Stage 198E exact
//! claim → commit → enqueue protocol (`sr_claim_endpoint_waiter_split` /
//! `sr_commit_blocked_receiver_split` / `sr_enqueue_committed_receiver_split`, with
//! `sr_restore_endpoint_waiter_split` for rollback). `ReplyCapRecord` remains the
//! sole reply authority; the owned [`AckLease`](super::ipccall_direct::AckLease)
//! governs single delivery.
//!
//! Ordering (Part 3/4): reserve → mint → bind → copy(payload+meta) → claim exact
//! waiter → commit server (Runnable, wake plan) → record `Reserved→Available` →
//! scheduler enqueue LAST. The record becomes `Available` before the rank-1 enqueue,
//! and a server cannot dispatch until enqueued — so a server never runs with a
//! `Reserved` record. No fallible op runs after the enqueue.

use crate::kernel::boot::KernelError;
use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
use crate::kernel::ipccall_direct::{AckLease, BlockedServerAck, IpcCallDirectSnapshot};
use crate::runtime::{ReceiverCommit, SharedKernel};

/// Success payload of a committed direct request transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IpcCallDirectSuccess {
    pub(crate) record_index: usize,
    pub(crate) record_generation: u64,
    pub(crate) server_reply_cap: CapId,
}

/// Failure classification. Every variant leaves the server blocked with a valid
/// waiter, no usable reply authority, and zero wake; retryable variants restore the
/// acknowledgement lease, terminal (server-gone) variants discard it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IpcCallDirectError {
    /// No exact committed blocked server — canonical `WouldBlock`, no mutation, no
    /// queued fallback. Lease restored.
    WouldBlock,
    /// The caller `{tid,asid}` no longer matches (replacement/exit). Lease restored.
    CallerGone,
    /// SEND endpoint cap resolution failed.
    SendEndpoint(KernelError),
    /// The reply-endpoint RECEIVE cap resolution failed.
    ReplyEndpoint(KernelError),
    /// The SEND endpoint generation changed vs. the acknowledgement. Lease restored.
    EndpointGenerationChanged,
    /// The reply-record table is full.
    RecordFull,
    /// The server has no registered cnode.
    ServerCnodeMissing,
    /// The provisional server-local reply-cap mint failed (e.g. server CNode full).
    MintFailed,
    /// The request payload copy to the server faulted. Server stays blocked/retryable.
    PayloadCopyFault,
    /// The recv-v2 metadata copy to the server faulted. Server stays blocked/retryable.
    MetaCopyFault,
    /// The exact endpoint waiter was changed/missing at claim time (slot untouched).
    WaiterLost,
    /// The server exited/was replaced after the claim — terminal, lease discarded.
    ServerGone,
    /// The infallible record commit unexpectedly failed (defensive; unreachable for
    /// an exact live reservation).
    RecordCommitFailed,
    /// The lease was not `ClaimedByWork` by this work item (duplicate/aliased drain).
    LeaseNotClaimed,
}

/// Bounded, owned post-work item published by the x86 trap-entry gate and drained
/// post-lock (Stage 199A2B2F). Contains ONLY owned data — the caller identity +
/// endpoint CapIds + payload bytes are inside `snapshot`, and the claimed
/// acknowledgement is captured by value with its `ack_seq` claim token. No userspace
/// payload pointer survives here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectRequestPostWork {
    pub(crate) snapshot: IpcCallDirectSnapshot,
    pub(crate) ack: BlockedServerAck,
    pub(crate) ack_seq: u64,
}

impl SharedKernel {
    /// Drain one owned direct-request post-work item post-lock: run the accepted
    /// transaction, then reconcile the GLOBAL published-acknowledgement claim with the
    /// in-transaction lease disposition — a retryable rollback (lease returned to
    /// `Available`) re-arms the published ack for another drain; success or a stale
    /// discard leaves it claimed (consumed). Does not duplicate the transaction body.
    pub(crate) fn drain_direct_request_post_work(
        &self,
        work: &DirectRequestPostWork,
    ) -> Result<IpcCallDirectSuccess, IpcCallDirectError> {
        let mut lease = AckLease::new_available();
        // The published ack was claimed at trap-entry publication; re-establish the
        // ClaimedByWork token for the transaction.
        let _ = lease.claim(work.ack_seq);
        let result =
            self.ipc_call_direct_request_txn(&work.snapshot, &work.ack, &mut lease, work.ack_seq);
        if lease.is_available() {
            // Retryable pre-claim rollback: re-arm the published acknowledgement.
            crate::kernel::boot::ipccall_direct_ack::restore(work.ack_seq);
        }
        result
    }

    /// True iff the EXACT original server is still committed-blocked and its endpoint
    /// waiter identity + generation are intact — the sole condition under which a
    /// rolled-back acknowledgement lease may be RESTORED (retryable). Any drift
    /// (server exited / incarnation changed / endpoint generation changed / waiter now
    /// another identity or missing) makes it `false`, so the lease is discarded.
    fn direct_server_exact_still_blocked(&self, ack: &BlockedServerAck) -> bool {
        self.sr_prevalidate_blocked_receiver_split(ack.server.tid.0, ack.server.asid)
            && self.endpoint_waiter_is_split_read(
                ack.endpoint_index,
                ack.endpoint_generation,
                ack.server,
            )
    }

    /// Settle the acknowledgement lease after a PRE-waiter-claim failure: restore it
    /// (retryable) only when the exact original server + waiter remain intact,
    /// otherwise discard it (a stale acknowledgement can never be resurrected).
    fn settle_lease_pre_claim(&self, ack: &BlockedServerAck, lease: &mut AckLease, seq: u64) {
        if self.direct_server_exact_still_blocked(ack) {
            let _ = lease.restore(seq);
        } else {
            lease.discard();
        }
    }

    /// Run the composed off-lock direct NR6 request transaction. `lease` must already
    /// be `ClaimedByWork { commit_seq: lease_commit_seq }` (claimed at post-work
    /// publication). On success the lease is `Consumed` and the server is enqueued
    /// exactly once; on failure the lease is restored (retryable) or discarded
    /// (server gone) and every provisional artifact is reclaimed.
    pub(crate) fn ipc_call_direct_request_txn(
        &self,
        snapshot: &IpcCallDirectSnapshot,
        ack: &BlockedServerAck,
        lease: &mut AckLease,
        lease_commit_seq: u64,
    ) -> Result<IpcCallDirectSuccess, IpcCallDirectError> {
        // (0) The acknowledgement must be committed/well-formed, else non-mutating
        // WouldBlock — never a queued fallback. Checked FIRST and touches NOTHING (the
        // lease is not fabricated or restored): in production no lease is claimed
        // without a committed acknowledgement.
        if !ack.is_committed() {
            return Err(IpcCallDirectError::WouldBlock);
        }

        // The lease must be held by THIS work item — a duplicate drain cannot proceed.
        if !matches!(lease, AckLease::ClaimedByWork { commit_seq } if *commit_seq == lease_commit_seq)
        {
            return Err(IpcCallDirectError::LeaseNotClaimed);
        }

        // (1) revalidate caller {tid,asid}.
        if self.task_asid_for_tid_split_read(snapshot.caller.tid.0) != snapshot.caller.asid.0 as u64
        {
            self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
            return Err(IpcCallDirectError::CallerGone);
        }

        // (2) resolve SEND endpoint; it must name the acknowledged request endpoint
        // (exact index + generation).
        let send_endpoint = match self
            .resolve_endpoint_send_cap_split_read(snapshot.caller.tid.0, snapshot.send_endpoint_cap)
        {
            Ok(o) => o,
            Err(e) => {
                self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
                return Err(IpcCallDirectError::SendEndpoint(e));
            }
        };
        match send_endpoint {
            CapObject::Endpoint { index, generation }
                if index == ack.endpoint_index && generation == ack.endpoint_generation => {}
            CapObject::Endpoint { index, .. } if index == ack.endpoint_index => {
                self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
                return Err(IpcCallDirectError::EndpointGenerationChanged);
            }
            _ => {
                self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
                return Err(IpcCallDirectError::WouldBlock);
            }
        }

        // (3) resolve caller reply-endpoint RECEIVE cap → reply endpoint object.
        let reply_endpoint = match self.resolve_endpoint_recv_cap_split_read(
            snapshot.caller.tid.0,
            snapshot.reply_endpoint_cap,
        ) {
            Ok(snap) => snap.endpoint,
            Err(e) => {
                self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
                return Err(IpcCallDirectError::ReplyEndpoint(e));
            }
        };

        // (4) require the exact committed blocked server (still Blocked(EndpointReceive)
        // with the exact {tid,asid}). No mutation, no queued fallback on a miss.
        if !self.sr_prevalidate_blocked_receiver_split(ack.server.tid.0, ack.server.asid) {
            self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
            return Err(IpcCallDirectError::WouldBlock);
        }

        // (5) reserve one ReplyCapRecord slot (Reserved → NOT externally invokable).
        let (idx, rgen) = match self.reserve_direct_reply_record_split(
            snapshot.caller,
            ack.server,
            reply_endpoint,
        ) {
            Ok(v) => v,
            Err(_) => {
                self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
                return Err(IpcCallDirectError::RecordFull);
            }
        };
        let reply_object = CapObject::Reply {
            index: idx,
            generation: rgen,
        };

        // (6) mint exactly one provisional server-local Reply cap.
        let server_cnode = match self.process_cnode_for_identity_split_read(ack.server) {
            Some(c) => c,
            None => {
                let _ = self.cancel_direct_reply_record_split(idx, rgen);
                self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
                return Err(IpcCallDirectError::ServerCnodeMissing);
            }
        };
        let server_cap = match self
            .sr_mint_split(server_cnode, Capability::new(reply_object, CapRights::SEND))
        {
            Ok(c) => c,
            Err(_) => {
                let _ = self.cancel_direct_reply_record_split(idx, rgen);
                self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
                return Err(IpcCallDirectError::MintFailed);
            }
        };

        // (7) bind the provisional cap to the reserved record (infallible for ours).
        self.bind_direct_reply_record_server_cap_split(idx, rgen, server_cap);

        // (8) copy request payload + recv-v2 metadata to the server, OUTSIDE all locks.
        let server_asid_raw = ack.server.asid.0 as u64;
        if self
            .copy_slice_to_user_asid_split_write(
                server_asid_raw,
                ack.payload_user_ptr,
                snapshot.payload(),
            )
            .is_err()
        {
            self.sr_revoke_split(server_cnode, server_cap, reply_object);
            let _ = self.cancel_direct_reply_record_split(idx, rgen);
            self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
            return Err(IpcCallDirectError::PayloadCopyFault);
        }
        let meta = crate::kernel::syscall::ipc_recv_core::encode_recv_v2_meta(
            0,
            crate::kernel::syscall::OPCODE_INLINE,
            crate::kernel::ipc::Message::FLAG_REPLY_CAP,
            snapshot.payload_len as u32,
            server_cap.0,
            1, // recv_meta_flags: reply-cap-present (bit 0)
            snapshot.caller.tid.0,
        );
        if self
            .copy_slice_to_user_asid_split_write(server_asid_raw, ack.meta_user_ptr, &meta)
            .is_err()
        {
            self.sr_revoke_split(server_cnode, server_cap, reply_object);
            let _ = self.cancel_direct_reply_record_split(idx, rgen);
            self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
            return Err(IpcCallDirectError::MetaCopyFault);
        }

        // (9) atomically claim the EXACT endpoint waiter (remove once). A changed /
        // missing waiter leaves the slot untouched.
        let claim = match self.sr_claim_endpoint_waiter_split(
            ack.endpoint_index,
            ack.endpoint_generation,
            ack.server,
        ) {
            Some(c) => c,
            None => {
                self.sr_revoke_split(server_cnode, server_cap, reply_object);
                let _ = self.cancel_direct_reply_record_split(idx, rgen);
                self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
                return Err(IpcCallDirectError::WaiterLost);
            }
        };

        // (10) commit the blocked server (Runnable + wake plan). Registers are cleared
        // ONLY here, strictly after the claim.
        match self.sr_commit_blocked_receiver_split(ack.server.tid.0, ack.server.asid) {
            ReceiverCommit::Committed(affinity) => {
                // (11) record Reserved → Available — INFALLIBLE for our exact live
                // reservation. Runs before the rank-1 enqueue, so the record is
                // Available before the server can dispatch.
                if !self.commit_direct_reply_record_split(idx, rgen) {
                    // Defensive (unreachable): the server is Runnable but NOT enqueued,
                    // so it cannot dispatch. Reclaim everything; discard the lease.
                    self.sr_revoke_split(server_cnode, server_cap, reply_object);
                    let _ = self.cancel_direct_reply_record_split(idx, rgen);
                    lease.discard();
                    return Err(IpcCallDirectError::RecordCommitFailed);
                }
                // (12) scheduler enqueue LAST — the single, non-fallible wake.
                self.sr_enqueue_committed_receiver_split(ack.server.tid.0, affinity);
                // (13) consume the acknowledgement lease exactly once.
                let _ = lease.consume(lease_commit_seq);
                Ok(IpcCallDirectSuccess {
                    record_index: idx,
                    record_generation: rgen,
                    server_reply_cap: server_cap,
                })
            }
            // Server exited / was replaced after the identity claim: the claimed waiter
            // belonged to the vanished incarnation and MUST NOT be restored (a restore
            // could only target the gone incarnation). Reclaim; discard the ack; zero
            // wake. `claim` is intentionally dropped (waiter left removed).
            ReceiverCommit::GoneDead | ReceiverCommit::Replaced => {
                let _ = claim;
                self.sr_revoke_split(server_cnode, server_cap, reply_object);
                let _ = self.cancel_direct_reply_record_split(idx, rgen);
                lease.discard();
                Err(IpcCallDirectError::ServerGone)
            }
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Stage 199A2B3 — the composed OFF-LOCK NR7 `IpcReplyDirect` transaction.
// ═════════════════════════════════════════════════════════════════════════════

use crate::kernel::ipccall_direct::{BlockedCallerAck, IpcReplyDirectSnapshot};

/// Success payload of a committed direct reply transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IpcReplyDirectSuccess {
    pub(crate) record_index: usize,
    pub(crate) record_generation: u64,
}

/// Failure classification for the NR7 direct reply. Every variant leaves the caller
/// blocked, no duplicate wake, and either restores the acknowledgement (exact caller
/// retryable) or discards it (stale authority).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IpcReplyDirectError {
    /// No committed caller acknowledgement — canonical `WouldBlock`, no mutation.
    WouldBlock,
    /// The reply cap did not resolve to a live `Reply` object.
    ReplyCapResolve(KernelError),
    /// The reservation precondition failed (generation mismatch, wrong bound replier,
    /// a non-`Available` / aliased record, or a reservation-precondition violation).
    ReservePreconditionFailed,
    /// The exact caller reply-endpoint waiter was changed/missing.
    WaiterLost,
    /// The caller exited / was replaced (before or after the claim).
    CallerGone,
    /// The reply payload copy to the caller faulted.
    PayloadCopyFault,
    /// The recv-v2 metadata copy to the caller faulted.
    MetaCopyFault,
    /// The one-shot record consume unexpectedly failed (defensive; unreachable for an
    /// owned reservation).
    RecordConsumeFailed,
    /// The lease was not `ClaimedByWork` by this work item (duplicate/aliased drain).
    LeaseNotClaimed,
}

/// Bounded, owned NR7 reply post-work item (Stage 199A2B3). Owned data only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectReplyPostWork {
    pub(crate) snapshot: IpcReplyDirectSnapshot,
    pub(crate) ack: BlockedCallerAck,
    pub(crate) ack_seq: u64,
}

impl SharedKernel {
    /// True iff the EXACT caller is still committed-blocked on its reply endpoint with
    /// the intact waiter identity + generation — the sole condition under which a
    /// caller-copy-fault rollback may restore usable reply authority + the ack.
    fn direct_caller_exact_still_blocked(&self, ack: &BlockedCallerAck) -> bool {
        self.sr_prevalidate_blocked_receiver_split(ack.caller.tid.0, ack.caller.asid)
            && self.endpoint_waiter_is_split_read(
                ack.endpoint_index,
                ack.endpoint_generation,
                ack.caller,
            )
    }

    /// Run the composed off-lock NR7 direct reply transaction. `lease` must already be
    /// `ClaimedByWork { commit_seq: lease_commit_seq }`. Source payload was copied at
    /// trap entry (into `snapshot`) BEFORE any record claim. On success the record is
    /// `Consumed` (the one-shot barrier), the caller is enqueued exactly once, and the
    /// lease is consumed; on failure the reservation + lease are settled by the
    /// exact-caller policy.
    pub(crate) fn ipc_reply_direct_txn(
        &self,
        snapshot: &IpcReplyDirectSnapshot,
        ack: &BlockedCallerAck,
        lease: &mut AckLease,
        lease_commit_seq: u64,
    ) -> Result<IpcReplyDirectSuccess, IpcReplyDirectError> {
        // (0) committed ack, else non-mutating WouldBlock (no lease touch).
        if !ack.is_committed() {
            return Err(IpcReplyDirectError::WouldBlock);
        }
        if !matches!(lease, AckLease::ClaimedByWork { commit_seq } if *commit_seq == lease_commit_seq)
        {
            return Err(IpcReplyDirectError::LeaseNotClaimed);
        }

        // (1) resolve the reply object {index, generation} the replier's cap names.
        let (idx, rgen) =
            match self.resolve_reply_cap_split_read(snapshot.replier.tid.0, snapshot.reply_cap) {
                Ok(v) => v,
                Err(e) => {
                    self.settle_reply_pre_reserve(ack, lease, lease_commit_seq);
                    return Err(IpcReplyDirectError::ReplyCapResolve(e));
                }
            };

        // (2) require the exact caller reply-endpoint waiter (fast pre-check).
        if !self.endpoint_waiter_is_split_read(
            ack.endpoint_index,
            ack.endpoint_generation,
            ack.caller,
        ) {
            self.settle_reply_pre_reserve(ack, lease, lease_commit_seq);
            return Err(IpcReplyDirectError::WaiterLost);
        }

        // (3) reserve the EXISTING record Available→Reserved (bound replier + exact
        // generation enforced). The source payload is ALREADY owned in `snapshot`, so
        // the claim happens strictly AFTER the source copy. An alias / non-Available /
        // wrong-replier record fails here.
        if !self.reserve_existing_reply_record_split(idx, rgen, snapshot.replier) {
            self.settle_reply_pre_reserve(ack, lease, lease_commit_seq);
            return Err(IpcReplyDirectError::ReservePreconditionFailed);
        }

        // (4) copy the owned reply payload + recv-v2 metadata to the caller OFF-LOCK.
        let caller_asid_raw = ack.caller.asid.0 as u64;
        if self
            .copy_slice_to_user_asid_split_write(
                caller_asid_raw,
                ack.payload_user_ptr,
                snapshot.payload(),
            )
            .is_err()
        {
            self.settle_reply_after_reserve(ack, idx, rgen, lease, lease_commit_seq);
            return Err(IpcReplyDirectError::PayloadCopyFault);
        }
        let meta = crate::kernel::syscall::ipc_recv_core::encode_recv_v2_meta(
            0,
            crate::kernel::syscall::OPCODE_INLINE,
            0,
            snapshot.payload_len as u32,
            0,
            0,
            snapshot.replier.tid.0,
        );
        if self
            .copy_slice_to_user_asid_split_write(caller_asid_raw, ack.meta_user_ptr, &meta)
            .is_err()
        {
            self.settle_reply_after_reserve(ack, idx, rgen, lease, lease_commit_seq);
            return Err(IpcReplyDirectError::MetaCopyFault);
        }

        // (5) atomically claim the EXACT caller waiter (remove once).
        let claim = match self.sr_claim_endpoint_waiter_split(
            ack.endpoint_index,
            ack.endpoint_generation,
            ack.caller,
        ) {
            Some(c) => c,
            None => {
                self.settle_reply_after_reserve(ack, idx, rgen, lease, lease_commit_seq);
                return Err(IpcReplyDirectError::WaiterLost);
            }
        };

        // (6) commit the blocked caller (Runnable + wake plan) strictly after the claim.
        match self.sr_commit_blocked_receiver_split(ack.caller.tid.0, ack.caller.asid) {
            ReceiverCommit::Committed(affinity) => {
                // (7) record Reserved → Consumed — the authoritative one-shot barrier,
                // BEFORE the rank-1 enqueue. Infallible for our exact reservation.
                if !self.consume_reply_record_split(idx, rgen) {
                    // Defensive/unreachable: caller Runnable but not enqueued (cannot
                    // dispatch). Discard record + ack; zero wake.
                    let _ = self.discard_reply_record_split(idx, rgen);
                    lease.discard();
                    return Err(IpcReplyDirectError::RecordConsumeFailed);
                }
                // (8) enqueue the caller LAST — the single, non-fallible wake.
                self.sr_enqueue_committed_receiver_split(ack.caller.tid.0, affinity);
                let _ = lease.consume(lease_commit_seq);
                Ok(IpcReplyDirectSuccess {
                    record_index: idx,
                    record_generation: rgen,
                })
            }
            // Caller exited / replaced after the claim: the claimed waiter belonged to
            // the vanished incarnation — do NOT restore it. Consume the record (barrier),
            // discard the ack; zero wake.
            ReceiverCommit::GoneDead | ReceiverCommit::Replaced => {
                let _ = claim;
                let _ = self.discard_reply_record_split(idx, rgen);
                lease.discard();
                Err(IpcReplyDirectError::CallerGone)
            }
        }
    }

    /// Settle after a PRE-reservation failure (no record reserved): restore the ack
    /// only when the exact caller remains retryable, else discard.
    fn settle_reply_pre_reserve(&self, ack: &BlockedCallerAck, lease: &mut AckLease, seq: u64) {
        if self.direct_caller_exact_still_blocked(ack) {
            let _ = lease.restore(seq);
        } else {
            lease.discard();
        }
    }

    /// Settle after the record is `Reserved` (caller-copy fault / waiter lost): for an
    /// exact still-blocked caller, `Reserved → Available` (reply authority stays usable)
    /// and restore the ack; for stale authority, `Reserved → Consumed` (permanently
    /// non-invokable) and discard the ack. Zero wake in both cases.
    fn settle_reply_after_reserve(
        &self,
        ack: &BlockedCallerAck,
        idx: usize,
        rgen: u64,
        lease: &mut AckLease,
        seq: u64,
    ) {
        if self.direct_caller_exact_still_blocked(ack) {
            let _ = self.release_reply_record_split(idx, rgen);
            let _ = lease.restore(seq);
        } else {
            let _ = self.discard_reply_record_split(idx, rgen);
            lease.discard();
        }
    }

    /// Drain one owned NR7 reply post-work item post-lock: run the transaction, then
    /// reconcile the published caller-ack claim with the in-transaction lease (retryable
    /// rollback re-arms the ack; success/stale-discard leaves it claimed).
    pub(crate) fn drain_direct_reply_post_work(
        &self,
        work: &DirectReplyPostWork,
    ) -> Result<IpcReplyDirectSuccess, IpcReplyDirectError> {
        let mut lease = AckLease::new_available();
        let _ = lease.claim(work.ack_seq);
        let result = self.ipc_reply_direct_txn(&work.snapshot, &work.ack, &mut lease, work.ack_seq);
        if lease.is_available() {
            crate::kernel::boot::ipcreply_direct_ack::restore(work.ack_seq);
        }
        result
    }
}
