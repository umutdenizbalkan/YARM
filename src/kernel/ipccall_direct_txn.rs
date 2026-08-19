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
use crate::runtime::{ReceiverCommit, ReceiverEnqueue, SharedKernel};

/// Success payload of a committed direct request transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IpcCallDirectSuccess {
    pub(crate) record_index: usize,
    pub(crate) record_generation: u64,
    pub(crate) server_reply_cap: CapId,
    /// The CPU the woken server was **actually enqueued on**, as reported by the rank-1 enqueue
    /// itself. This is the authority for the post-transaction wake decision: comparing it to the
    /// enqueueing CPU is what distinguishes a local enqueue (no IPI) from a genuine remote one
    /// (exactly one IPI, aimed here). It is never assumed and never derived from a selector.
    pub(crate) wake_target_cpu: crate::kernel::scheduler::CpuId,
}

/// Failure classification. Every variant leaves the server blocked with a valid
/// waiter, no usable reply authority, and zero wake; retryable variants restore the
/// acknowledgement lease, terminal (server-gone) variants discard it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Constructed on freestanding targets, where the transaction runs; a hosted `lib`
// build compiles no route to it, so the variants look unconstructed there.
#[allow(dead_code)]
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
    /// Stage 199D: the final rank-1 placement was REFUSED, carrying the scheduler's OWN
    /// reason rather than collapsing five different facts into one. The whole publication is
    /// rolled back — the server is `Blocked` on its exact original recv cap with its waiter
    /// restored once, holds no scheduler membership, and owns no record, cap or link — and NO
    /// success, and therefore no wake target, exists. Retryable.
    EnqueueRejected(crate::kernel::scheduler::SchedulerError),
    /// Stage 199D: the placement was refused with **pre-existing scheduler membership** that
    /// could not be atomically reconciled — the receiver is `current` (it may already have
    /// observed the publication), duplicated, or vanished from the queue it was reported in.
    /// The externally visible authority (record, cap, link) is reclaimed, but **no claim is
    /// made that the receiver was restored**: it is deliberately left as found rather than
    /// forced `Blocked` on top of live scheduler membership. Terminal.
    EnqueueRejectedUnreconciled(crate::kernel::scheduler::WithdrawOutcome),
    /// Stage 199D: the receiver held scheduler membership while still `Blocked` with a committed
    /// waiter — an INVARIANT VIOLATION, detected before the first userspace-observable mutation.
    /// Nothing was copied, minted, bound, reserved, claimed or transitioned, and the
    /// acknowledgement is DISCARDED rather than re-armed. Fail-closed and terminal: no claim is
    /// made that the receiver is restored or unplaced.
    ReceiverMembershipViolation,
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
    /// `executing_cpu` is the CPU actually running this drain, threaded explicitly from the
    /// trap/post-lock boundary (`try_split_ipccall_direct_into_frame`'s `cpu`). It is the sole
    /// authority for the remote-wake decision below; the process-global ambient
    /// `scheduler.current_cpu` is never consulted.
    pub(crate) fn drain_direct_request_post_work(
        &self,
        executing_cpu: crate::kernel::scheduler::CpuId,
        work: &DirectRequestPostWork,
    ) -> Result<IpcCallDirectSuccess, IpcCallDirectError> {
        // The remote-wake decision below is x86_64-freestanding only; everywhere else the
        // explicit CPU is deliberately unused. Discarded here rather than renamed, so the
        // parameter keeps its contract name at every call site.
        #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
        let _ = executing_cpu;
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
        if let Ok(success) = result {
            // Stage 199A2B4: a genuine off-lock request delivery completed — emit the NR6
            // live success + retirement markers (one-shot; no-op unless the x86 oracle
            // feature+selector are both active).
            crate::kernel::boot::emit_ipccall_direct_request_live_markers();
            // Stage 199D — the post-enqueue REMOTE-WAKE decision.
            //
            // The woken server was enqueued on `success.wake_target_cpu`, reported by the rank-1
            // enqueue itself. A wake IPI is needed only when that target is a DIFFERENT CPU from
            // the one running this drain: a local enqueue lands on our own run queue and the
            // ordinary dispatcher picks it up.
            //
            // This decision used to read `x86_ipccall_direct_smp_request_enabled()` — a global
            // oracle selector — and unconditionally aim at a hardcoded CPU 1. While the NR6
            // production default was off, the oracle's single request was the only traffic that
            // reached here, so it fired once and looked correct. Once the default was enabled
            // (`fcfc55e3`), EVERY ordinary direct request in the boot fired it too: 53 ordinary
            // local completions plus the one genuine CPU0→CPU1 oracle delivery = the 54 remote-wake
            // IPIs the seal rejected. The selector was never authority for "is this wake remote?".
            //
            // Now the enqueue's own committed target decides, so ordinary local traffic sends
            // nothing regardless of any selector, and a real remote enqueue is woken on its
            // authoritative home CPU rather than an assumed one. Strictly after the enqueue commit:
            // the transaction has returned `Ok` and no fallible work follows.
            //
            // U3 (canonical 203C): the "which CPU is running this drain?" half of that comparison
            // used to read the process-global ambient `scheduler.current_cpu`. That is not this
            // drain's identity: it is a single field any CPU may retarget, so two otherwise
            // identical `-smp 2` boots disagreed about whether the very same enqueue was remote.
            // It is now `executing_cpu`, threaded explicitly from the trap boundary. There is no
            // ambient fallback: if the caller has no CPU there is no drain.
            let _ = success;
            #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
            {
                let enqueueing_cpu = executing_cpu;
                if success.wake_target_cpu != enqueueing_cpu {
                    crate::kernel::boot::ipccall_direct_smp_request_note_delivered();
                    crate::arch::x86_64::smp::send_reschedule_ipi_to(
                        enqueueing_cpu,
                        success.wake_target_cpu,
                    );
                }
            }
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
            // Stage 199D: `Blocked` + an intact waiter is NOT sufficient. A task that is also
            // queued or current is in an inconsistent state, and re-arming an acknowledgement
            // against it would hand that state to the next drain.
            && !self.receiver_has_scheduler_membership_split_read(ack.server.tid.0)
    }

    /// Stage 199D — the COMPLETE rollback of a direct-request publication that has already
    /// passed the waiter claim and the receiver commit.
    ///
    /// Undoes every mutation in exact reverse order of publication:
    ///
    /// | # | published | undone by |
    /// |---|---|---|
    /// | 11b | reverse link registered | `unregister_server_reply_link_split` (idempotent; a no-op if it was never registered) |
    /// | 11 | record `Reserved → Available` | `cancel_direct_reply_record_split` |
    /// | 6/7 | provisional server-local reply cap | `sr_revoke_split` |
    /// | 10 | server `Blocked → Runnable` | `sr_uncommit_blocked_receiver_split` |
    /// | 9 | endpoint waiter claimed | `sr_restore_endpoint_waiter_split` |
    ///
    /// Order matters twice. The record and cap go first, so no reply authority is reachable at
    /// any instant when the server could be scheduled — it cannot be, since it is never enqueued,
    /// but the ordering makes that independent of the enqueue. And the receiver must be returned
    /// to `Blocked` **before** the waiter is restored: `sr_restore_endpoint_waiter_split`
    /// prevalidates that the task is exactly blocked, so restoring first would simply fail.
    ///
    /// The payload and metadata already copied into the server's buffers at (8) are not undone —
    /// they cannot be. They are also never *observed*: the server returns to `Blocked` with no
    /// reply cap and no record, so it never returns from `recv` on the strength of them, and
    /// whichever delivery or timeout eventually completes it overwrites those same buffers.
    ///
    /// Returns `true` iff the server is exactly blocked again with its waiter reinstalled — the
    /// condition under which the acknowledgement may be RESTORED for a later retry rather than
    /// discarded. The caller settles the lease; this never touches it.
    #[allow(clippy::too_many_arguments)]
    fn rollback_direct_request_after_commit(
        &self,
        ack: &BlockedServerAck,
        claim: &crate::runtime::WaiterClaim,
        server_recv_cap: Option<CapId>,
        server_cnode: crate::kernel::capabilities::CNodeId,
        server_cap: CapId,
        reply_object: CapObject,
        idx: usize,
        rgen: u64,
    ) -> bool {
        let _ =
            self.unregister_server_reply_link_split(ack.server.tid.0, ack.server.asid, idx, rgen);
        let _ = self.cancel_direct_reply_record_split(idx, rgen);
        self.sr_revoke_split(server_cnode, server_cap, reply_object);
        let Some(recv_cap) = server_recv_cap else {
            // The wait reason was not captured, so the exact blocked state cannot be
            // reconstructed. Everything externally visible is still reclaimed above; the
            // acknowledgement will be discarded rather than restored.
            return false;
        };
        if !self.sr_uncommit_blocked_receiver_split(ack.server.tid.0, ack.server.asid, recv_cap) {
            return false;
        }
        self.sr_restore_endpoint_waiter_split(claim)
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

        // (4a) Stage 199D — the EARLY membership check, before the FIRST userspace-observable
        // mutation. Everything below this point is observable: (5) publishes a record another
        // transaction can see, (6)/(7) put a provisional capability in the SERVER's own cnode,
        // and (8) writes the server's user memory. A check placed after those cannot support
        // retry or authority restoration, because a receiver reported `RefusedCurrent` may
        // already be executing and may already have read them.
        //
        // A legitimate `Blocked` receiver holding a committed waiter cannot acquire scheduler
        // membership — nothing can wake it while both are true — so a positive result here is an
        // INVARIANT VIOLATION, not a retryable `WouldBlock`. Nothing has been mutated, so there
        // is nothing to unwind; the acknowledgement is DISCARDED rather than re-armed, because
        // re-arming would hand the same broken state to the next drain.
        if self.receiver_has_scheduler_membership_split_read(ack.server.tid.0) {
            lease.discard();
            crate::yarm_log!(
                "IPC_DIRECT_REQUEST_MEMBERSHIP_VIOLATION server_tid={} phase=pre_mutation result=failed_closed",
                ack.server.tid.0
            );
            return Err(IpcCallDirectError::ReceiverMembershipViolation);
        }

        // (4b) Stage 200D-1: RESERVE reverse-link capacity BEFORE anything becomes
        // externally visible. The record slot below is `Reserved` (not invokable), but
        // failing the link registration only at step (11b) would mean the record had
        // already been published `Available` in the failing window. Probing here keeps the
        // externally atomic order: both resources are known-available before either is
        // published, so every failure from this point leaves zero live records, zero live
        // links, no visible request and no enqueued server.
        //
        // The probe also rejects a server incarnation that has already committed to exit,
        // so an exiting server can never have a request exposed to it.
        if !self.can_reserve_server_reply_link_split(ack.server.tid.0, ack.server.asid) {
            self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
            return Err(IpcCallDirectError::RecordFull);
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
        //
        // Stage 199D HARD-STOP A: what the server observes is the CANONICAL receiver-visible
        // projection, not the raw snapshot. `handle_ipc_call` frames the kernel message as
        // `Message::with_header(caller_tid, OPCODE_INLINE, FLAG_REPLY_CAP, …, payload)`, and
        // userspace `ipc_call` prepends `[app_opcode_le(2)]` to that payload — so the legacy
        // blocked-waiter reply-cap delivery strips the prefix and reports the APPLICATION
        // opcode. Projecting the same header words through the same rule here makes the
        // off-lock delivery byte-identical to the legacy one: same opcode, same payload
        // bytes, same length, same meta words. Delivering the raw snapshot instead handed
        // the server opcode 0 and a payload shifted by two bytes.
        let delivery = crate::kernel::syscall::ipc_recv_core::project_recv_delivery_parts(
            crate::kernel::syscall::OPCODE_INLINE,
            crate::kernel::ipc::Message::FLAG_REPLY_CAP,
            snapshot.caller.tid.0,
            snapshot.payload(),
        );
        let server_asid_raw = ack.server.asid.0 as u64;
        if self
            .copy_slice_to_user_asid_split_write(
                server_asid_raw,
                ack.payload_user_ptr,
                delivery.app_payload,
            )
            .is_err()
        {
            self.sr_revoke_split(server_cnode, server_cap, reply_object);
            let _ = self.cancel_direct_reply_record_split(idx, rgen);
            self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
            return Err(IpcCallDirectError::PayloadCopyFault);
        }
        // Blocked-waiter meta: `status` and `msg_flags` are 0, `cap_id` is the freshly
        // minted SERVER-LOCAL reply cap, `recv_meta_flags` carries the reply-cap bit —
        // exactly the words `runtime.rs`'s blocked-waiter reply-cap executor writes.
        let meta =
            delivery.encode_blocked_waiter_meta(server_cap.0, delivery.reply_cap_recv_meta_flags());
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

        // (9b) Stage 199D: capture the server's wait reason BEFORE the commit clears it, so a
        // refused enqueue at (12) can restore the exact `Blocked(EndpointReceive(cap))` it had.
        // Read-only (rank 2). Its absence would mean the server is no longer exactly blocked,
        // which (10) is about to reject anyway.
        let server_recv_cap = self.blocked_recv_cap_split_read(ack.server.tid.0, ack.server.asid);

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
                // Stage 200D (11b) — register the BOUNDED reverse link from the exact
                // server incarnation to the record it now owes, so teardown can find it
                // by index instead of scanning every reply record.
                //
                // Ordering is deliberate: the record is already `Available` (fully
                // initialized and authoritative), and the server is Runnable but NOT yet
                // enqueued, so it cannot have dispatched and no NR7 can be in flight. The
                // link therefore cannot miss a window.
                //
                // Capacity is one outstanding record. A second registration FAILS rather
                // than silently overwriting, and we then roll the whole publication back
                // BEFORE the request becomes externally visible — the server is never
                // enqueued, so userspace observes nothing.
                if !self.register_server_reply_link_split(
                    ack.server.tid.0,
                    ack.server.asid,
                    idx,
                    rgen,
                ) {
                    // Stage 199D: this leaves the SAME state a refused enqueue does — server
                    // Runnable, waiter claimed, record Available — so it runs the SAME complete
                    // rollback rather than a second, weaker one. (It previously reclaimed the cap
                    // and record but left the server Runnable-but-unqueued with its waiter
                    // removed.) The link registration failed, and unregistering is idempotent by
                    // construction, so the shared helper is safe here.
                    let restored = self.rollback_direct_request_after_commit(
                        ack,
                        &claim,
                        server_recv_cap,
                        server_cnode,
                        server_cap,
                        reply_object,
                        idx,
                        rgen,
                    );
                    self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
                    crate::yarm_log!(
                        "IPC_SERVER_REPLY_LINK_REGISTER_FAIL server_tid={} record_index={} reason=capacity restored={} result=rolled_back",
                        ack.server.tid.0,
                        idx,
                        u32::from(restored)
                    );
                    return Err(IpcCallDirectError::RecordCommitFailed);
                }
                // (12) scheduler enqueue LAST — the single wake. It reports what it ACTUALLY
                //      did, and only `Enqueued` may become this success's wake target.
                let outcome =
                    self.sr_enqueue_committed_receiver_reconciled_split(ack.server.tid.0, affinity);
                let ReceiverEnqueue::Enqueued {
                    cpu: wake_target_cpu,
                } = outcome
                else {
                    // Stage 199D — the placement was REFUSED after the publication committed.
                    //
                    // The externally visible authority is reclaimed either way. Whether the
                    // server may additionally be returned to `Blocked` depends on whether it
                    // provably holds NO scheduler membership: `receiver_is_unplaced()` is true
                    // for the four reasons that never touch a queue, and for `AlreadyQueued`
                    // only when the same-rank-1-acquisition reconciliation removed exactly one
                    // queued entry. A `current`, duplicated or vanished membership fails closed
                    // — forcing `Blocked` on top of live membership would create a `Blocked`
                    // task that is still queued or running, and would claim a restoration that
                    // did not happen.
                    let ReceiverEnqueue::Rejected {
                        error, reconciled, ..
                    } = outcome
                    else {
                        unreachable!("Enqueued was matched above")
                    };
                    if !outcome.rejection_is_runtime_recoverable() {
                        let _ = self.unregister_server_reply_link_split(
                            ack.server.tid.0,
                            ack.server.asid,
                            idx,
                            rgen,
                        );
                        let _ = self.cancel_direct_reply_record_split(idx, rgen);
                        self.sr_revoke_split(server_cnode, server_cap, reply_object);
                        lease.discard();
                        crate::yarm_log!(
                            "IPC_DIRECT_REQUEST_ENQUEUE_UNRECONCILED server_tid={} record_index={} error={:?} membership={:?} restored=0 result=failed_closed",
                            ack.server.tid.0,
                            idx,
                            error,
                            reconciled
                        );
                        return Err(IpcCallDirectError::EnqueueRejectedUnreconciled(
                            reconciled
                                .unwrap_or(crate::kernel::scheduler::WithdrawOutcome::NotQueued),
                        ));
                    }
                    // Roll the whole publication back, in exact reverse order, so the server is
                    // returned to the retryable state it held when this transaction began rather
                    // than left Runnable-but-unqueued (unschedulable, and — since a direct-
                    // eligible request has no armed reply timeout — with no terminal owner).
                    let restored = self.rollback_direct_request_after_commit(
                        ack,
                        &claim,
                        server_recv_cap,
                        server_cnode,
                        server_cap,
                        reply_object,
                        idx,
                        rgen,
                    );
                    // Stage 199D — RECOVERABLE, including `AlreadyQueued` reconciled as
                    // `Removed`. `Removed` proves exactly one queued entry was withdrawn under
                    // the same rank-1 acquisition that detected it, and that the task was NOT
                    // `current` — so it never ran and never observed the publication. Treating
                    // every reconciled outcome as terminal was over-broad.
                    //
                    // `receiver_is_unplaced()` is the single predicate: true for the four
                    // reasons that never touched a queue, and for `AlreadyQueued` only on
                    // `Removed`. Everything else took the fail-closed branch above, so a
                    // variant documented as retryable is never returned after its lease was
                    // discarded.
                    self.settle_lease_pre_claim(ack, lease, lease_commit_seq);
                    crate::yarm_log!(
                        "IPC_DIRECT_REQUEST_ENQUEUE_REJECTED server_tid={} record_index={} error={:?} restored={} result=rolled_back",
                        ack.server.tid.0,
                        idx,
                        error,
                        u32::from(restored)
                    );
                    return Err(IpcCallDirectError::EnqueueRejected(error));
                };
                // (13) consume the acknowledgement lease exactly once.
                let _ = lease.consume(lease_commit_seq);
                Ok(IpcCallDirectSuccess {
                    record_index: idx,
                    record_generation: rgen,
                    server_reply_cap: server_cap,
                    wake_target_cpu,
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
    /// The CPU the woken caller was **actually enqueued on**, reported by the rank-1 enqueue.
    /// The reverse-direction twin of `IpcCallDirectSuccess::wake_target_cpu`, and the authority
    /// for the reverse remote-wake decision.
    pub(crate) wake_target_cpu: crate::kernel::scheduler::CpuId,
}

/// Failure classification for the NR7 direct reply. Every variant leaves the caller
/// blocked, no duplicate wake, and either restores the acknowledgement (exact caller
/// retryable) or discards it (stale authority).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Constructed on freestanding targets, where the transaction runs; a hosted `lib`
// build compiles no route to it, so the variants look unconstructed there.
#[allow(dead_code)]
pub(crate) enum IpcReplyDirectError {
    /// No committed caller acknowledgement — canonical `WouldBlock`, no mutation.
    WouldBlock,
    /// The reply cap did not resolve to a live `Reply` object.
    ReplyCapResolve(KernelError),
    /// The reservation precondition failed (generation mismatch, wrong bound replier,
    /// a non-`Available` / aliased record, or a reservation-precondition violation).
    ReservePreconditionFailed,
    /// The exact caller reply-endpoint waiter was changed/missing at the PRE-reserve
    /// check — before any reservation and before any user copy, so nothing was delivered
    /// and the legacy path may still run.
    WaiterLost,
    /// The exact caller reply-endpoint waiter was changed/missing at the CLAIM, which runs
    /// strictly after the reply payload AND metadata have already been copied into the
    /// caller's address space. Distinct from [`Self::WaiterLost`] precisely because the
    /// post-states differ: this one can never be a legacy fallback (see
    /// `crate::kernel::direct_disposition`).
    WaiterLostAfterCopy,
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
    /// Stage 199D: the final rank-1 placement was REFUSED, carrying the scheduler's OWN
    /// reason. The one-shot reply authority is RESTORED to its exact pre-transaction state —
    /// record `Consumed → Available` at the same generation and bound replier, reverse link
    /// re-registered, ack lease restored — the caller is `Blocked` on its exact original recv
    /// cap with its waiter restored once, and NO success exists. Retryable: the same reply may
    /// be re-sent and will succeed exactly once.
    EnqueueRejected(crate::kernel::scheduler::SchedulerError),
    /// Stage 199D: refused with pre-existing scheduler membership that could not be atomically
    /// reconciled (see `IpcCallDirectError::EnqueueRejectedUnreconciled`). The reply authority
    /// is NOT restored — a caller that may already have observed the delivery must not have a
    /// second reply armed against it — and no claim is made that the caller was restored.
    /// Terminal.
    EnqueueRejectedUnreconciled(crate::kernel::scheduler::WithdrawOutcome),
    /// Stage 199D: see `IpcCallDirectError::ReceiverMembershipViolation`. Detected before the
    /// record reservation and before the caller copy; nothing was mutated and the acknowledgement
    /// is discarded. Fail-closed and terminal.
    ReceiverMembershipViolation,
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
            // Stage 199D: see `direct_server_exact_still_blocked` — scheduler membership
            // disqualifies restoration even with a Blocked status and an intact waiter.
            && !self.receiver_has_scheduler_membership_split_read(ack.caller.tid.0)
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

        // (2b) Stage 199D — the EARLY membership check, before the FIRST userspace-observable
        // mutation. (3) below moves the record to a state another transaction can observe and
        // (4) writes the caller's user memory; a check after either cannot support authority
        // restoration, because a `RefusedCurrent` caller may already have read the reply.
        // Nothing is mutated here, so nothing is unwound; the acknowledgement is DISCARDED.
        if self.receiver_has_scheduler_membership_split_read(ack.caller.tid.0) {
            lease.discard();
            crate::yarm_log!(
                "IPC_DIRECT_REPLY_MEMBERSHIP_VIOLATION caller_tid={} phase=pre_mutation result=failed_closed",
                ack.caller.tid.0
            );
            return Err(IpcReplyDirectError::ReceiverMembershipViolation);
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
                // POST-copy: the reply payload and metadata are already in the caller's
                // address space, so this can never fall back to the legacy reply path.
                return Err(IpcReplyDirectError::WaiterLostAfterCopy);
            }
        };

        // (5b) Stage 199D: capture the caller's wait reason BEFORE the commit clears it, so a
        // refused enqueue at (8) can restore the exact `Blocked(EndpointReceive(cap))` it had.
        let caller_recv_cap = self.blocked_recv_cap_split_read(ack.caller.tid.0, ack.caller.asid);

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
                // (8) enqueue the caller LAST — the single wake. It reports what it ACTUALLY
                //     did, and only `Enqueued` may become this success's wake target.
                let outcome =
                    self.sr_enqueue_committed_receiver_reconciled_split(ack.caller.tid.0, affinity);
                let ReceiverEnqueue::Enqueued {
                    cpu: wake_target_cpu,
                } = outcome
                else {
                    // Stage 199D — the placement was REFUSED after the record was consumed.
                    //
                    // There is NO reply-timeout owner to fall back on. A direct-eligible reply
                    // is by construction one whose record is NOT terminal-arbitrated —
                    // `classify_direct_reply` declines `terminal_arbitrated` before any
                    // mutation — and the arbitration flag is exactly "a reply timeout is armed
                    // for this record incarnation". So the whole direct-eligible population is
                    // untimed, and leaving the caller `Blocked` with the record spent would
                    // strand it with no terminal owner at all.
                    //
                    // Route A: restore the EXACT one-shot authority so the same reply retries.
                    // Admissible only when the caller provably holds no scheduler membership —
                    // otherwise it may already have observed the delivery, and re-arming a
                    // second reply against it would be unsound.
                    let ReceiverEnqueue::Rejected {
                        error, reconciled, ..
                    } = outcome
                    else {
                        unreachable!("Enqueued was matched above")
                    };
                    if !outcome.rejection_is_runtime_recoverable() {
                        lease.discard();
                        crate::yarm_log!(
                            "IPC_DIRECT_REPLY_ENQUEUE_UNRECONCILED caller_tid={} record_index={} error={:?} membership={:?} authority_restored=0 result=failed_closed",
                            ack.caller.tid.0,
                            idx,
                            error,
                            reconciled
                        );
                        return Err(IpcReplyDirectError::EnqueueRejectedUnreconciled(
                            reconciled
                                .unwrap_or(crate::kernel::scheduler::WithdrawOutcome::NotQueued),
                        ));
                    }
                    let caller_restored = caller_recv_cap.is_some_and(|cap| {
                        self.sr_uncommit_blocked_receiver_split(
                            ack.caller.tid.0,
                            ack.caller.asid,
                            cap,
                        )
                    }) && self.sr_restore_endpoint_waiter_split(&claim);
                    // Stage 199D — RECOVERABLE, including `AlreadyQueued` reconciled as
                    // `Removed`: the caller provably never became `current`, so the delivery was
                    // not observed and the exact one-shot authority may be re-armed. The
                    // fail-closed branch above already took every other outcome.
                    let authority_restored = caller_restored
                        && self.restore_consumed_reply_record_split(idx, rgen, snapshot.replier);
                    if authority_restored {
                        // Exact pre-transaction state: the reply may be re-sent and will succeed
                        // exactly once, so the acknowledgement is restored, not discarded.
                        self.settle_reply_pre_reserve(ack, lease, lease_commit_seq);
                    } else {
                        lease.discard();
                    }
                    crate::yarm_log!(
                        "IPC_DIRECT_REPLY_ENQUEUE_REJECTED caller_tid={} record_index={} error={:?} caller_restored={} authority_restored={} result=rolled_back",
                        ack.caller.tid.0,
                        idx,
                        error,
                        u32::from(caller_restored),
                        u32::from(authority_restored)
                    );
                    return Err(IpcReplyDirectError::EnqueueRejected(error));
                };
                let _ = lease.consume(lease_commit_seq);
                Ok(IpcReplyDirectSuccess {
                    record_index: idx,
                    record_generation: rgen,
                    wake_target_cpu,
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
    /// `executing_cpu` is the CPU actually running this drain, threaded explicitly from the
    /// trap/post-lock boundary (`try_split_ipcreply_direct_into_frame`'s `cpu`) — the exact
    /// mirror of the NR6 twin. It is the sole authority for the reverse remote-wake decision.
    pub(crate) fn drain_direct_reply_post_work(
        &self,
        executing_cpu: crate::kernel::scheduler::CpuId,
        work: &DirectReplyPostWork,
    ) -> Result<IpcReplyDirectSuccess, IpcReplyDirectError> {
        // See the NR6 twin: the reverse decision is x86_64-freestanding only.
        #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
        let _ = executing_cpu;
        let mut lease = AckLease::new_available();
        let _ = lease.claim(work.ack_seq);
        let result = self.ipc_reply_direct_txn(&work.snapshot, &work.ack, &mut lease, work.ack_seq);
        if lease.is_available() {
            crate::kernel::boot::ipcreply_direct_ack::restore(work.ack_seq);
        }
        if let Ok(success) = result {
            // Stage 199A2B4: a genuine off-lock reply delivery completed — emit the NR7 live
            // success + retirement markers (one-shot; no-op unless the x86 oracle
            // feature+selector are both active).
            crate::kernel::boot::emit_ipcreply_direct_live_markers();
            // Stage 199D — the post-enqueue REVERSE remote-wake decision, the exact mirror of the
            // NR6 twin in `drain_direct_request_post_work`.
            //
            // The woken caller was enqueued on `success.wake_target_cpu`. A wake IPI is needed
            // only when that target is a DIFFERENT CPU from the one running this drain; a local
            // enqueue lands on our own run queue and the ordinary dispatcher picks it up.
            //
            // This used to read `x86_ipccall_direct_smp_reply_enabled()` — a global oracle
            // selector — and aim at a hardcoded CPU 0, so every ordinary direct NR7 completion
            // fired a reverse wake once the NR6/NR7 production default was on. That produced a
            // spurious extra `X86_BSP_RESCHEDULE_IPI_SENT` before the oracle's own reply, which
            // RUN_D rejects (it requires exactly one). Strictly after the enqueue commit: the
            // transaction has returned `Ok` and no fallible work follows. The target sets its own
            // pending flag on IPI receipt (never a self-set from here) and dispatches through its
            // normal scheduler — no dispatch in the IPI handler.
            //
            // U3 (canonical 203C): as in the NR6 twin, the executing-CPU half of the comparison is
            // now the explicitly threaded `executing_cpu` rather than the process-global ambient
            // `scheduler.current_cpu`. No ambient fallback.
            let _ = success;
            #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
            {
                let enqueueing_cpu = executing_cpu;
                if success.wake_target_cpu != enqueueing_cpu {
                    crate::kernel::boot::ipcreply_direct_smp_reply_note_delivered();
                    crate::arch::x86_64::smp::c2c_send_reschedule_ipi_to(
                        enqueueing_cpu,
                        success.wake_target_cpu,
                    );
                }
            }
        }
        result
    }
}
