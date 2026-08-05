// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199D HARD-STOP B — the typed disposition contract for direct NR6/NR7 outcomes.
//!
//! The split-dispatch helpers used to discard the transaction result
//! (`let _ = shared.drain_direct_request_post_work(&work);`) and unconditionally write
//! `frame.set_ok(0, 0, 0)`. Every failure the transaction classifies — a gone server, a full
//! record table, a failed mint, a faulted user copy, a lost waiter — was therefore reported
//! to userspace as **success with no message delivered**. On the oracle path no failure ever
//! occurs, so it was invisible; on a production boot it is silent message loss with no error
//! and no fallback.
//!
//! This module replaces that with one pure, exhaustive mapping per direction. There is no
//! wildcard arm: adding a variant to either error enum is a compile error until its
//! disposition is decided deliberately.
//!
//! # The three dispositions
//!
//! * [`DirectDisposition::Completed`] — the transaction committed. The success frame is
//!   encoded by [`apply_direct_disposition`], which reproduces the legacy return lanes
//!   exactly (Stage 199D HARD-STOP C).
//! * [`DirectDisposition::DeclinedBeforeMutation`] — nothing observable changed and nothing
//!   was delivered, so the syscall may run its legacy path. The helper returns `None`.
//! * [`DirectDisposition::Failed`] — something irreversible happened, or a user copy was
//!   attempted. The legacy path must NOT run; the canonical error legacy would have produced
//!   is encoded into the frame with `set_err`, which is byte-for-byte how the global-lock
//!   handler encodes a `SyscallError` (`fault_state.rs`: `trapframe.set_err(e.code())`).
//!
//! # When fallback is permitted
//!
//! `DeclinedBeforeMutation` is admissible **only** when all six of these hold of the state
//! the transaction leaves behind:
//!
//! 1. no reply record reserved or committed,
//! 2. no capability minted or installed,
//! 3. no user payload or metadata copied,
//! 4. no waiter or run queue changed,
//! 5. no acknowledgement committed as a delivery,
//! 6. no wake published.
//!
//! Two clarifications, because both are load-bearing:
//!
//! * Conditions 1 and 2 are judged on the state the transaction **leaves**, not on whether a
//!   provisional step ran. A reply-record reservation is `Reserved` — never externally
//!   invokable — and a provisional mint is revoked on the same path that created it, so a
//!   completed rollback genuinely leaves no record and no capability. Where a rollback is
//!   only partial, the variant is `Failed`.
//! * Condition 5 is about *delivery*, not about the acknowledgement lease. An acknowledgement
//!   that was claimed and then discarded (rather than restored) published nothing; it only
//!   forfeits the direct path's own retry, which fails safe — a later NR6 for that endpoint
//!   finds nothing claimable and declines to legacy.
//!
//! Condition 3 is deliberately strict: a user copy that *faulted* may have written a prefix
//! of its bytes, so any attempted copy disqualifies fallback even if the rollback is
//! otherwise complete. This also matches what legacy does — `complete_blocked_recv_for_waiter`
//! returns `InvalidArgs` on a copy fault rather than falling through to a queued send.
//!
//! # Why never fall through after publication
//!
//! Once the endpoint waiter has been claimed, the request bytes are in the server's address
//! space, or the receiver has been committed `Runnable`, running the legacy path afterwards
//! would deliver the same message a second time — a duplicate delivery and a duplicate wake.
//! Every variant past that line is `Failed`, unconditionally.

use crate::kernel::ipccall_direct_txn::{
    IpcCallDirectError, IpcCallDirectSuccess, IpcReplyDirectError, IpcReplyDirectSuccess,
};
use crate::kernel::syscall::{SYSCALL_NO_TRANSFER_CAP, SyscallError};
use crate::kernel::trapframe::TrapFrame;

/// Apply a disposition to the trap frame and report whether the syscall was handled.
///
/// `Some(())` — handled; the split helper returns `Some(Ok(()))`.
/// `None` — declined; the frame is **untouched** and the legacy path runs.
///
/// This is the single frame-encoding site for both directions, so the return-lane contract
/// cannot drift between NR6 and NR7 or between the success and failure arms. Hosted tests
/// drive this exact function, which is why it is not inlined into the (freestanding-only)
/// split helpers.
///
/// # Stage 199D HARD-STOP C — the success lanes
///
/// The legacy `handle_ipc_call` and `handle_ipc_reply` BOTH end their success path with
///
/// ```text
/// frame.set_ok(0, 0, 0);
/// encode_transfer_cap_ret(frame, None)?;   // → set_ret2(SYSCALL_NO_TRANSFER_CAP)
/// ```
///
/// so the legacy success frame is `error = 0`, `ret0 = 0`, `ret1 = 0`,
/// `ret2 = SYSCALL_NO_TRANSFER_CAP` (`u64::MAX`) — *not* zero. The direct path previously
/// wrote `set_ok(0, 0, 0)` alone, leaving `ret2 = 0`: a silent divergence from the legacy
/// ABI on the **successful** path. It now writes the same three lanes, taking the sentinel
/// from [`SYSCALL_NO_TRANSFER_CAP`] — the very constant `encode_transfer_cap_ret` writes —
/// rather than duplicating the literal.
///
/// # Failure and decline
///
/// `Failed` encodes with `set_err`, which zeroes `ret0`/`ret1`/`ret2` before setting the
/// error code, so a failure can never leave a stale success value in any return lane —
/// including the `ret2` sentinel this fix introduces. `DeclinedBeforeMutation` writes
/// nothing at all: the legacy path is entitled to a pristine frame.
#[allow(dead_code)] // freestanding-live; hosted `lib` compiles no route to the split helpers
pub(crate) fn apply_direct_disposition(
    frame: &mut TrapFrame,
    disposition: DirectDisposition,
) -> Option<()> {
    match disposition {
        DirectDisposition::Completed => {
            // Byte-identical to the legacy `set_ok(0, 0, 0)` + `encode_transfer_cap_ret(None)`.
            frame.set_ok(0, 0, SYSCALL_NO_TRANSFER_CAP as usize);
            Some(())
        }
        DirectDisposition::DeclinedBeforeMutation => None,
        DirectDisposition::Failed(err) => {
            frame.set_err(err.code());
            Some(())
        }
    }
}

/// What the trap-entry gate must do with a direct-transaction outcome.
///
/// Live on freestanding targets, where the split helpers match on it; unreachable in a
/// hosted `lib` build, which compiles no route to those helpers.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectDisposition {
    /// The transaction committed; keep the existing successful frame encoding.
    Completed,
    /// Nothing observable changed and nothing was delivered: the legacy path may run.
    DeclinedBeforeMutation,
    /// Irreversible (or attempted-copy) failure: encode this canonical error and do NOT
    /// let the legacy path run.
    Failed(SyscallError),
}

// `permits_legacy_fallback` and `failure` are the contract's two invariant probes: the
// split helpers match on the enum directly, so these are consumed by the disposition tests
// and by the fault-injection suite rather than by production code. They are part of the
// contract in every build.
#[allow(dead_code)]
impl DirectDisposition {
    /// True iff the legacy path is allowed to run after this outcome.
    pub(crate) fn permits_legacy_fallback(self) -> bool {
        matches!(self, Self::DeclinedBeforeMutation)
    }

    /// The canonical error to encode, if any.
    pub(crate) fn failure(self) -> Option<SyscallError> {
        match self {
            Self::Failed(err) => Some(err),
            Self::Completed | Self::DeclinedBeforeMutation => None,
        }
    }
}

/// The exhaustive disposition of a direct **NR6 request** outcome.
///
/// Pure: no kernel state, no locks, no logging. The rationale for each arm, and the legacy
/// outcome it is equivalent to:
///
/// | Variant | Disposition | State left behind | Legacy equivalent |
/// |---|---|---|---|
/// | `WouldBlock` | Declined | pristine; ack restored | legacy queues the message or blocks the caller |
/// | `LeaseNotClaimed` | Declined | pristine (a duplicate drain touched nothing) | legacy re-runs the send |
/// | `CallerGone` | Declined | pristine; no cap resolved | legacy resolves for a vanished caller and raises the canonical cap error |
/// | `SendEndpoint(_)` | Declined | pristine | `validate_endpoint_right(cap, SEND)` raises `InvalidCapability` / `MissingRight` / `WrongObject` |
/// | `ReplyEndpoint(_)` | Declined | pristine | `validate_endpoint_right(reply_cap, RECEIVE)` raises the same class |
/// | `EndpointGenerationChanged` | Declined | pristine | legacy re-resolves the endpoint from the cap |
/// | `RecordFull` | Declined | pristine (the reverse-link probe or the reservation refused) | `create_reply_cap_for_caller` raises the table-full error |
/// | `ServerCnodeMissing` | Declined | reservation cancelled; nothing minted or copied | legacy fails later in materialization |
/// | `MintFailed` | Declined | reservation cancelled, provisional mint revoked | `materialize_received_message_cap` raises `CapabilityFull` |
/// | `PayloadCopyFault` | Failed(`InvalidArgs`) | a user copy was attempted | `complete_blocked_recv_for_waiter` returns `InvalidArgs` |
/// | `MetaCopyFault` | Failed(`InvalidArgs`) | a user copy was attempted | same, after rolling the materialized cap back |
/// | `WaiterLost` | Failed(`WrongObject`) | request payload **and** metadata are already in the server's address space | the acknowledged waiter is not the endpoint's waiter — a stale-authority error |
/// | `ServerGone` | Failed(`ServerDied`) | the endpoint waiter was claimed and deliberately not restored | the request will never be answered by that incarnation — exactly `ServerDied`'s contract |
/// | `RecordCommitFailed` | Failed(`Internal`) | the server is committed `Runnable` | defensive; unreachable for an exact live reservation |
///
/// Note `WaiterLost` sits *after* step (8) in the transaction, so the server's payload and
/// metadata destinations have already been written; it can never be a fallback.
#[allow(dead_code)] // freestanding-live; hosted `lib` compiles no route to the split helpers
pub(crate) fn classify_direct_request_outcome(
    outcome: &Result<IpcCallDirectSuccess, IpcCallDirectError>,
) -> DirectDisposition {
    let err = match outcome {
        Ok(_) => return DirectDisposition::Completed,
        Err(err) => err,
    };
    match err {
        // ── Fallback-eligible: nothing delivered, nothing left behind ────────────────
        IpcCallDirectError::WouldBlock
        | IpcCallDirectError::LeaseNotClaimed
        | IpcCallDirectError::CallerGone
        | IpcCallDirectError::SendEndpoint(_)
        | IpcCallDirectError::ReplyEndpoint(_)
        | IpcCallDirectError::EndpointGenerationChanged
        | IpcCallDirectError::RecordFull
        | IpcCallDirectError::ServerCnodeMissing
        | IpcCallDirectError::MintFailed => DirectDisposition::DeclinedBeforeMutation,

        // ── A user copy was attempted: legacy raises InvalidArgs, never falls through ──
        IpcCallDirectError::PayloadCopyFault | IpcCallDirectError::MetaCopyFault => {
            DirectDisposition::Failed(SyscallError::InvalidArgs)
        }

        // ── Past the publication line: falling through would deliver twice ───────────
        IpcCallDirectError::WaiterLost => DirectDisposition::Failed(SyscallError::WrongObject),
        IpcCallDirectError::ServerGone => DirectDisposition::Failed(SyscallError::ServerDied),
        // Stage 199D: the publication is rolled back COMPLETELY (record cancelled, cap
        // revoked, link unregistered, server returned to Blocked with its waiter reinstalled),
        // so nothing was delivered and nothing leaked. It is still NOT fallback-eligible: it
        // sits after step (8), so the server's payload/metadata destinations have already been
        // written, and this file's standing rule is that anything past the copy line never
        // falls through. The kernel could not PLACE the wake — that is an internal condition,
        // not a userspace error.
        IpcCallDirectError::RecordCommitFailed
        | IpcCallDirectError::EnqueueRejected(_)
        | IpcCallDirectError::EnqueueRejectedUnreconciled(_) => {
            DirectDisposition::Failed(SyscallError::Internal)
        }
    }
}

/// The exhaustive disposition of a direct **NR7 reply** outcome.
///
/// | Variant | Disposition | State left behind | Legacy equivalent |
/// |---|---|---|---|
/// | `WouldBlock` | Declined | pristine | legacy `ipc_reply` runs unchanged |
/// | `LeaseNotClaimed` | Declined | pristine (duplicate drain) | legacy re-runs the reply |
/// | `ReplyCapResolve(_)` | Declined | pristine | legacy resolves the reply cap and raises the canonical error |
/// | `ReservePreconditionFailed` | Declined | pristine (the reservation refused) | legacy `ipc_reply` raises the one-shot/stale-record error |
/// | `WaiterLost` | Declined | pristine — this is the **pre-reserve, pre-copy** check | legacy re-resolves the caller waiter |
/// | `PayloadCopyFault` | Failed(`InvalidArgs`) | a user copy was attempted; reservation settled | legacy reply copy fault returns `InvalidArgs` |
/// | `MetaCopyFault` | Failed(`InvalidArgs`) | a user copy was attempted; reservation settled | same |
/// | `WaiterLostAfterCopy` | Failed(`WrongObject`) | the reply payload **and** metadata are already in the caller's address space | stale reply authority |
/// | `CallerGone` | Failed(`WrongObject`) | the caller waiter was claimed and deliberately not restored; record discarded | the reply cap names a caller that no longer exists |
/// | `RecordConsumeFailed` | Failed(`Internal`) | the caller is committed `Runnable` | defensive; unreachable for an owned reservation |
///
/// The two `WaiterLost` positions in the transaction have **different** post-states, so they
/// are different variants: step (2) runs before any reservation or copy and is fallback-safe;
/// step (5) runs after both copies have already landed in the caller's address space and can
/// never be.
#[allow(dead_code)] // freestanding-live; hosted `lib` compiles no route to the split helpers
pub(crate) fn classify_direct_reply_outcome(
    outcome: &Result<IpcReplyDirectSuccess, IpcReplyDirectError>,
) -> DirectDisposition {
    let err = match outcome {
        Ok(_) => return DirectDisposition::Completed,
        Err(err) => err,
    };
    match err {
        // ── Fallback-eligible: nothing delivered, nothing left behind ────────────────
        IpcReplyDirectError::WouldBlock
        | IpcReplyDirectError::LeaseNotClaimed
        | IpcReplyDirectError::ReplyCapResolve(_)
        | IpcReplyDirectError::ReservePreconditionFailed
        | IpcReplyDirectError::WaiterLost => DirectDisposition::DeclinedBeforeMutation,

        // ── A user copy was attempted ───────────────────────────────────────────────
        IpcReplyDirectError::PayloadCopyFault | IpcReplyDirectError::MetaCopyFault => {
            DirectDisposition::Failed(SyscallError::InvalidArgs)
        }

        // ── Past the publication line ──────────────────────────────────────────────
        IpcReplyDirectError::WaiterLostAfterCopy | IpcReplyDirectError::CallerGone => {
            DirectDisposition::Failed(SyscallError::WrongObject)
        }
        // Stage 199D: the record stays `Consumed` (one-shot spent, never re-armed) and the
        // caller is returned to Blocked with its waiter reinstalled, so nothing leaks and no
        // second reply is possible. Past the copy line, so never a fallback.
        IpcReplyDirectError::RecordConsumeFailed
        | IpcReplyDirectError::EnqueueRejected(_)
        | IpcReplyDirectError::EnqueueRejectedUnreconciled(_) => {
            DirectDisposition::Failed(SyscallError::Internal)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::KernelError;
    use crate::kernel::boot::ReceiverWaiterIdentity;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::vm::Asid;

    fn request_success() -> Result<IpcCallDirectSuccess, IpcCallDirectError> {
        Ok(IpcCallDirectSuccess {
            record_index: 0,
            record_generation: 1,
            server_reply_cap: crate::kernel::capabilities::CapId(9),
            // Local enqueue: the committed target is the enqueueing CPU, so no remote wake.
            wake_target_cpu: crate::kernel::scheduler::CpuId(0),
        })
    }

    fn reply_success() -> Result<IpcReplyDirectSuccess, IpcReplyDirectError> {
        Ok(IpcReplyDirectSuccess {
            record_index: 0,
            record_generation: 1,
            // Local enqueue: the committed target is the enqueueing CPU, so no remote wake.
            wake_target_cpu: crate::kernel::scheduler::CpuId(0),
        })
    }

    /// EVERY request variant, with its pinned disposition. A new variant will not compile
    /// until it is added to the mapping AND listed here.
    fn request_variant_table() -> [(IpcCallDirectError, DirectDisposition); 14] {
        use DirectDisposition::*;
        use IpcCallDirectError as E;
        [
            (E::WouldBlock, DeclinedBeforeMutation),
            (E::LeaseNotClaimed, DeclinedBeforeMutation),
            (E::CallerGone, DeclinedBeforeMutation),
            (
                E::SendEndpoint(KernelError::MissingRight),
                DeclinedBeforeMutation,
            ),
            (
                E::ReplyEndpoint(KernelError::InvalidCapability),
                DeclinedBeforeMutation,
            ),
            (E::EndpointGenerationChanged, DeclinedBeforeMutation),
            (E::RecordFull, DeclinedBeforeMutation),
            (E::ServerCnodeMissing, DeclinedBeforeMutation),
            (E::MintFailed, DeclinedBeforeMutation),
            (E::PayloadCopyFault, Failed(SyscallError::InvalidArgs)),
            (E::MetaCopyFault, Failed(SyscallError::InvalidArgs)),
            (E::WaiterLost, Failed(SyscallError::WrongObject)),
            (E::ServerGone, Failed(SyscallError::ServerDied)),
            (E::RecordCommitFailed, Failed(SyscallError::Internal)),
        ]
    }

    /// EVERY reply variant, with its pinned disposition.
    fn reply_variant_table() -> [(IpcReplyDirectError, DirectDisposition); 10] {
        use DirectDisposition::*;
        use IpcReplyDirectError as E;
        [
            (E::WouldBlock, DeclinedBeforeMutation),
            (E::LeaseNotClaimed, DeclinedBeforeMutation),
            (
                E::ReplyCapResolve(KernelError::WrongObject),
                DeclinedBeforeMutation,
            ),
            (E::ReservePreconditionFailed, DeclinedBeforeMutation),
            (E::WaiterLost, DeclinedBeforeMutation),
            (E::PayloadCopyFault, Failed(SyscallError::InvalidArgs)),
            (E::MetaCopyFault, Failed(SyscallError::InvalidArgs)),
            (E::WaiterLostAfterCopy, Failed(SyscallError::WrongObject)),
            (E::CallerGone, Failed(SyscallError::WrongObject)),
            (E::RecordConsumeFailed, Failed(SyscallError::Internal)),
        ]
    }

    #[test]
    fn request_success_is_completed() {
        assert_eq!(
            classify_direct_request_outcome(&request_success()),
            DirectDisposition::Completed
        );
        assert!(
            !classify_direct_request_outcome(&request_success()).permits_legacy_fallback(),
            "a completed transaction must never fall through to legacy"
        );
    }

    #[test]
    fn reply_success_is_completed() {
        assert_eq!(
            classify_direct_reply_outcome(&reply_success()),
            DirectDisposition::Completed
        );
        assert!(!classify_direct_reply_outcome(&reply_success()).permits_legacy_fallback());
    }

    /// The mutation guard for the request mapping: every arm is pinned individually, so
    /// changing any one of them fails here.
    #[test]
    fn every_request_variant_has_its_pinned_disposition() {
        for (variant, expected) in request_variant_table() {
            assert_eq!(
                classify_direct_request_outcome(&Err(variant)),
                expected,
                "request variant {variant:?}"
            );
        }
    }

    /// The mutation guard for the reply mapping.
    #[test]
    fn every_reply_variant_has_its_pinned_disposition() {
        for (variant, expected) in reply_variant_table() {
            assert_eq!(
                classify_direct_reply_outcome(&Err(variant)),
                expected,
                "reply variant {variant:?}"
            );
        }
    }

    /// The disposition tables enumerate the enums exhaustively: the counts are pinned, and
    /// a duplicate entry would collapse the count.
    #[test]
    fn variant_tables_are_exhaustive_and_duplicate_free() {
        let req = request_variant_table();
        assert_eq!(req.len(), 14, "request enum variant count");
        for (i, (a, _)) in req.iter().enumerate() {
            for (b, _) in req.iter().skip(i + 1) {
                assert_ne!(
                    core::mem::discriminant(a),
                    core::mem::discriminant(b),
                    "duplicate request variant in the table"
                );
            }
        }
        let rep = reply_variant_table();
        assert_eq!(rep.len(), 10, "reply enum variant count");
        for (i, (a, _)) in rep.iter().enumerate() {
            for (b, _) in rep.iter().skip(i + 1) {
                assert_ne!(
                    core::mem::discriminant(a),
                    core::mem::discriminant(b),
                    "duplicate reply variant in the table"
                );
            }
        }
    }

    /// No failure disposition may ever permit fallback, and no decline may carry an error.
    #[test]
    fn failure_and_fallback_are_mutually_exclusive() {
        for (variant, expected) in request_variant_table() {
            let d = classify_direct_request_outcome(&Err(variant));
            assert_eq!(d, expected);
            assert_eq!(
                d.permits_legacy_fallback(),
                d.failure().is_none() && d != DirectDisposition::Completed,
                "request {variant:?}: fallback iff no canonical error"
            );
        }
        for (variant, expected) in reply_variant_table() {
            let d = classify_direct_reply_outcome(&Err(variant));
            assert_eq!(d, expected);
            assert_eq!(
                d.permits_legacy_fallback(),
                d.failure().is_none() && d != DirectDisposition::Completed,
                "reply {variant:?}: fallback iff no canonical error"
            );
        }
    }

    /// Every post-publication variant fails, and none of them is `WouldBlock` — a
    /// `WouldBlock` there would tell userspace to retry a request that was half-delivered.
    #[test]
    fn post_publication_variants_never_report_wouldblock_or_fall_through() {
        use IpcCallDirectError as Q;
        use IpcReplyDirectError as R;
        for variant in [
            Q::PayloadCopyFault,
            Q::MetaCopyFault,
            Q::WaiterLost,
            Q::ServerGone,
            Q::RecordCommitFailed,
        ] {
            let d = classify_direct_request_outcome(&Err(variant));
            assert!(!d.permits_legacy_fallback(), "request {variant:?}");
            assert_ne!(d.failure(), Some(SyscallError::WouldBlock));
            assert!(d.failure().is_some());
        }
        for variant in [
            R::PayloadCopyFault,
            R::MetaCopyFault,
            R::WaiterLostAfterCopy,
            R::CallerGone,
            R::RecordConsumeFailed,
        ] {
            let d = classify_direct_reply_outcome(&Err(variant));
            assert!(!d.permits_legacy_fallback(), "reply {variant:?}");
            assert_ne!(d.failure(), Some(SyscallError::WouldBlock));
            assert!(d.failure().is_some());
        }
    }

    /// Copy faults map to the SAME error the legacy blocked-waiter completion raises, not to
    /// `PageFault` — legacy additionally faults the current task on a `PageFault` code, which
    /// the direct path must not trigger.
    #[test]
    fn copy_faults_match_the_legacy_blocked_waiter_error() {
        assert_eq!(
            classify_direct_request_outcome(&Err(IpcCallDirectError::PayloadCopyFault)).failure(),
            Some(SyscallError::InvalidArgs)
        );
        assert_eq!(
            classify_direct_request_outcome(&Err(IpcCallDirectError::MetaCopyFault)).failure(),
            Some(SyscallError::InvalidArgs)
        );
        assert_ne!(
            classify_direct_request_outcome(&Err(IpcCallDirectError::PayloadCopyFault)).failure(),
            Some(SyscallError::PageFault),
            "PageFault would make the global-lock handler fault the current task"
        );
        // The legacy completion's own error for a copy failure.
        let legacy = SyscallError::InvalidArgs;
        assert_eq!(legacy.code(), 2);
    }

    /// The two identities used by the fault-injection tests exist and are distinct — a guard
    /// against a fixture accidentally comparing a value to itself.
    #[test]
    fn waiter_identities_are_distinguishable() {
        let a = ReceiverWaiterIdentity::new(ThreadId(2), Asid(7));
        let b = ReceiverWaiterIdentity::new(ThreadId(2), Asid(8));
        assert_ne!(a, b, "asid participates in the waiter identity");
    }
}
