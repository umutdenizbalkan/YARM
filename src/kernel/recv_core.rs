// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 33+34: Canonical internal IPC receive engine.
//!
//! # Purpose
//!
//! All public receive ABIs (ipc_recv NR2, ipc_recv_timeout NR5, recv-v2,
//! future recv_shared_v3) are thin adapters over the types defined here.
//! No public syscall number is added in this stage; `SYSCALL_COUNT` remains 30.
//!
//! # Adapter map
//!
//! ```text
//! ipc_recv (NR 2)          ── RecvRequest::from_legacy_ipc_recv     ─┐
//! ipc_recv_timeout (NR 5)  ── RecvRequest::from_ipc_recv_timeout    ─┤
//! recv-v2                  ── RecvRequest::from_recv_v2             ─┤─► recv_core engine
//! legacy mapped receive    ── RecvRequest::from_legacy_mapped_recv  ─┤
//! future recv_shared_v3    ── RecvRequest::future_shared_v3 [test]  ─┘
//! ```
//!
//! # Stage 33+34 live scope
//!
//! - **LIVE**: kernel-task queued plain recv via [`try_recv_core_kernel_plain`].
//! - **FALLBACK** (global-lock): user-ASID, recv-v2 metadata, timeout/blocking,
//!   cap-transfer, reply-cap, sender-waiter refill, mapped receive.
//! - **HELPER-ONLY**: [`recv_shared_v3`] request/output validation scaffold.
//! - **NOT ADDED**: any new public syscall number.
//!
//! # User-ASID copy-failure semantics (Stage 33 formalization, Stage 36+37 live-enable)
//!
//! See `doc/KERNEL_LOCKING.md §52` for the Stage 33+34 copy-failure semantics
//! table, `doc/KERNEL_LOCKING.md §54` for the Stage 36 semantics proof and
//! live-enable documentation, and `doc/KERNEL_LOCKING.md §55` for the Stage 37
//! recv-v2 metadata writeback semantics proof.
//!
//! **Stage 36 change:** narrow user-ASID plain recv (no meta, no map_intent)
//! is now eligible for the split path via [`try_recv_core_user_plain`] and
//! [`execute_user_asid_plain_writeback`].  The copy-failure semantics are
//! **proven equivalent** to the full path: dequeue happens first (under
//! `ipc_state_lock`), then the copy runs after the lock is released.  A copy
//! fault consumes the message and records a user fault — identical to the
//! global-lock full path.  See `doc/KERNEL_LOCKING.md §54` for the proof table.
//!
//! **Stage 37 change:** user-ASID recv-v2 plain recv (V2 meta, no map_intent)
//! is now eligible for the split path via [`try_recv_core_user_plain_v2`] and
//! [`execute_user_asid_plain_v2_writeback`].  Meta is written first (matching
//! the full-path ordering), then payload.  All three failure modes (meta fault,
//! payload undersized, payload fault) are reproduced exactly.  See §55.
//!
//! **Stage 38+39 change:** sender-waiter refill for plain messages is now
//! handled correctly on the split path.  When [`ipc_try_recv_queued_plain_endpoint_only`]
//! returns `ReceivedWithSenderWake(msg, wake_tid)`, the canonical core functions
//! return [`RecvOutcome::Delivered`] with [`RecvSchedulerWakePlan::WakeSender`]
//! instead of [`RecvOutcome::FallbackRequired`].  The caller
//! (`try_split_recv_queued_plain_with_snapshot_locked`) applies the wake plan
//! BEFORE writeback — matching the full-path order.  See §56.
//!
//! All three failure blockers that remain on the fallback list: cap-transfer
//! messages (FLAG_CAP_TRANSFER / FLAG_REPLY_CAP), sender-waiters with
//! cap-transfer messages, and mapped/shared receive.  See §56.

use super::boot::{IpcEndpointRecvResult, IpcEndpointSplitRejectReason, KernelError, KernelState};
use super::capabilities::{CapId, CapObject};
use super::ipc::Message;

/// Minimum metadata buffer length for a recv-v2 request.
///
/// Mirrors `IPC_RECV_META_V2_ENCODED_LEN` in `syscall.rs` — do not change
/// these independently; they must stay equal.
pub(crate) const META_V2_MIN_LEN: usize = 40;

// ─── Request model ────────────────────────────────────────────────────────────

/// ABI variant that produced this receive request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvRequestKind {
    /// Legacy `ipc_recv` (NR 2) — no explicit timeout.
    LegacyRecv,
    /// `ipc_recv_timeout` (NR 5) — explicit timeout_ticks / deadline.
    TimedRecv,
    /// Non-blocking probe (`timeout_ticks == 0`, NR 5 fast-path).
    NonblockingProbe,
    /// Future `recv_shared_v3` — **helper-only / test-only**, never dispatched
    /// through a live public syscall number.
    SharedV3Future,
}

/// Where the received message payload should be delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvPayloadTarget {
    /// Kernel task: return payload in inline register words.  No user-memory
    /// copy is required or permitted.
    KernelRegister,
    /// User-ASID task: copy payload bytes to `(ptr, len)` in user address space.
    ///
    /// **Copy-failure blocker active.** This variant is constructed by the adapter
    /// functions for documentation purposes, but [`try_recv_core_kernel_plain`]
    /// returns `FallbackRequired(UserAsidCopySemantics)` whenever it is present.
    /// See `doc/KERNEL_LOCKING.md §52`.
    UserMemory { ptr: usize, len: usize },
}

/// Where receive metadata should be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvMetaTarget {
    /// No metadata expected (legacy `ipc_recv` without v2 meta pointer).
    None,
    /// recv-v2: 40-byte `IpcRecvMetaV2` struct at `(ptr, len)`.
    V2 { ptr: usize, len: usize },
    /// Future `recv_shared_v3`: versioned output record at `(ptr, len)`.
    V3Future { ptr: usize, len: usize },
}

/// How long the receiver is willing to wait for a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvBlockingPolicy {
    /// Return immediately; do not block even if the queue is empty.
    NoWait,
    /// Block indefinitely until a message arrives.
    WaitForever,
    /// Block until the absolute scheduler tick `deadline` expires.
    Deadline(u64),
}

/// Which message transfer types this receive may accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvTransferPolicy {
    /// Accept only plain (unflagged) messages. Cap-transfer, reply-cap, and
    /// shared-memory messages are all rejected.
    PlainOnly,
    /// Accept any transfer type the message carries.  Used by the global-lock
    /// full path which can handle materialization and mapping.
    LegacyFull,
}

/// Map intent for future shared-memory receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvMapIntent {
    /// No shared-memory mapping requested.
    None,
    /// Map the transferred region read-only.
    ReadOnly,
    /// Map the transferred region read-write.
    ReadWrite,
}

/// Canonical internal IPC receive request.
///
/// **Internal only** — not part of any public ABI. Constructed by the
/// adapter functions ([`RecvRequest::from_legacy_ipc_recv`] etc.) which
/// decode existing syscall frames into this representation without altering
/// the public register ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecvRequest {
    pub kind: RecvRequestKind,
    pub requester_tid: u64,
    pub recv_cap: CapId,
    pub payload_target: RecvPayloadTarget,
    pub meta_target: RecvMetaTarget,
    pub blocking: RecvBlockingPolicy,
    pub transfer: RecvTransferPolicy,
    pub map_intent: RecvMapIntent,
}

impl RecvRequest {
    /// Decode an `ipc_recv` (NR 2) syscall frame into a canonical request.
    ///
    /// `is_kernel_task` must be `true` when the requester has no user ASID
    /// (kernel task); `false` for a user-ASID task.
    pub(crate) fn from_legacy_ipc_recv(
        requester_tid: u64,
        recv_cap: CapId,
        payload_ptr: usize,
        payload_len: usize,
        meta_ptr: usize,
        meta_len: usize,
        is_kernel_task: bool,
    ) -> Self {
        let payload_target = if is_kernel_task {
            RecvPayloadTarget::KernelRegister
        } else {
            RecvPayloadTarget::UserMemory {
                ptr: payload_ptr,
                len: payload_len,
            }
        };
        let meta_target = if meta_ptr != 0 && meta_len >= META_V2_MIN_LEN {
            RecvMetaTarget::V2 {
                ptr: meta_ptr,
                len: meta_len,
            }
        } else {
            RecvMetaTarget::None
        };
        RecvRequest {
            kind: RecvRequestKind::LegacyRecv,
            requester_tid,
            recv_cap,
            payload_target,
            meta_target,
            blocking: RecvBlockingPolicy::WaitForever,
            transfer: RecvTransferPolicy::LegacyFull,
            map_intent: RecvMapIntent::None,
        }
    }

    /// Decode an `ipc_recv_timeout` (NR 5) syscall into a canonical request.
    ///
    /// - `timeout_ticks == 0` → [`RecvRequestKind::NonblockingProbe`] with
    ///   [`RecvBlockingPolicy::NoWait`].
    /// - `timeout_ticks > 0`  → [`RecvRequestKind::TimedRecv`] with
    ///   [`RecvBlockingPolicy::Deadline`] using `absolute_deadline`.
    pub(crate) fn from_ipc_recv_timeout(
        requester_tid: u64,
        recv_cap: CapId,
        payload_ptr: usize,
        payload_len: usize,
        timeout_ticks: u64,
        absolute_deadline: Option<u64>,
        is_kernel_task: bool,
    ) -> Self {
        let (kind, blocking) = if timeout_ticks == 0 {
            (
                RecvRequestKind::NonblockingProbe,
                RecvBlockingPolicy::NoWait,
            )
        } else {
            let dl = absolute_deadline.unwrap_or(u64::MAX);
            (RecvRequestKind::TimedRecv, RecvBlockingPolicy::Deadline(dl))
        };
        let payload_target = if is_kernel_task {
            RecvPayloadTarget::KernelRegister
        } else {
            RecvPayloadTarget::UserMemory {
                ptr: payload_ptr,
                len: payload_len,
            }
        };
        RecvRequest {
            kind,
            requester_tid,
            recv_cap,
            payload_target,
            meta_target: RecvMetaTarget::None,
            blocking,
            transfer: RecvTransferPolicy::LegacyFull,
            map_intent: RecvMapIntent::None,
        }
    }

    /// Decode a recv-v2 request into a canonical request.
    ///
    /// When `meta_ptr == 0` or `meta_len < META_V2_MIN_LEN` the meta target
    /// is `None` (treated as legacy recv without v2 metadata).
    pub(crate) fn from_recv_v2(
        requester_tid: u64,
        recv_cap: CapId,
        payload_ptr: usize,
        payload_len: usize,
        meta_ptr: usize,
        meta_len: usize,
        is_kernel_task: bool,
    ) -> Self {
        let payload_target = if is_kernel_task {
            RecvPayloadTarget::KernelRegister
        } else {
            RecvPayloadTarget::UserMemory {
                ptr: payload_ptr,
                len: payload_len,
            }
        };
        let meta_target = if meta_ptr != 0 && meta_len >= META_V2_MIN_LEN {
            RecvMetaTarget::V2 {
                ptr: meta_ptr,
                len: meta_len,
            }
        } else {
            RecvMetaTarget::None
        };
        RecvRequest {
            kind: RecvRequestKind::LegacyRecv,
            requester_tid,
            recv_cap,
            payload_target,
            meta_target,
            blocking: RecvBlockingPolicy::WaitForever,
            transfer: RecvTransferPolicy::LegacyFull,
            map_intent: RecvMapIntent::None,
        }
    }

    /// Decode a legacy mapped-receive request into a canonical request.
    ///
    /// The mapped-receive path currently lives entirely on the global-lock full
    /// path. This adapter is provided for documentation and future conversion;
    /// `plan_recv_core` will return `FallbackRequired` for this request shape
    /// until the user-ASID writeback semantics are resolved.
    pub(crate) fn from_legacy_mapped_recv(
        requester_tid: u64,
        recv_cap: CapId,
        payload_ptr: usize,
        payload_len: usize,
        map_intent: RecvMapIntent,
    ) -> Self {
        RecvRequest {
            kind: RecvRequestKind::LegacyRecv,
            requester_tid,
            recv_cap,
            payload_target: RecvPayloadTarget::UserMemory {
                ptr: payload_ptr,
                len: payload_len,
            },
            meta_target: RecvMetaTarget::None,
            blocking: RecvBlockingPolicy::WaitForever,
            transfer: RecvTransferPolicy::LegacyFull,
            map_intent,
        }
    }

    /// Build a future `recv_shared_v3` request descriptor.
    ///
    /// **Helper-only / test-only.** This adapter is NOT routed through any live
    /// syscall dispatch path and does NOT add a public syscall number.  It
    /// exists solely to model future v3 requests for design validation and
    /// adapter equivalence tests.
    #[cfg(test)]
    pub(crate) fn future_shared_v3(
        requester_tid: u64,
        recv_cap: CapId,
        payload_ptr: usize,
        payload_len: usize,
        metadata_ptr: usize,
        metadata_len: usize,
        map_intent: RecvMapIntent,
        timeout: Option<u64>,
    ) -> Self {
        let blocking = match timeout {
            None => RecvBlockingPolicy::WaitForever,
            Some(0) => RecvBlockingPolicy::NoWait,
            Some(dl) => RecvBlockingPolicy::Deadline(dl),
        };
        RecvRequest {
            kind: RecvRequestKind::SharedV3Future,
            requester_tid,
            recv_cap,
            payload_target: RecvPayloadTarget::UserMemory {
                ptr: payload_ptr,
                len: payload_len,
            },
            meta_target: RecvMetaTarget::V3Future {
                ptr: metadata_ptr,
                len: metadata_len,
            },
            blocking,
            transfer: RecvTransferPolicy::LegacyFull,
            map_intent,
        }
    }
}

// ─── Outcome model ────────────────────────────────────────────────────────────

/// Why the canonical receive engine delegated to the global-lock full path.
///
/// This enum is part of the documented copy-failure semantics table in
/// `doc/KERNEL_LOCKING.md §52`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    /// Receiver has a user ASID and the request shape is not narrow-plain-eligible:
    /// `map_intent != None` (mapped receive).  Plain user-ASID recv without
    /// map_intent is now `UserPlainEligible` (see §54); plain recv-v2 without
    /// map_intent is now `UserPlainV2Eligible` (see §55).  Only mapped/non-plain
    /// requests remain here.
    UserAsidCopySemantics,
    /// recv-v2 or v3-future metadata output pointer with a kernel-task receiver,
    /// or v3-future meta with a user-ASID receiver.  User-ASID + V2 meta +
    /// no map_intent is promoted to [`RecvPlan::UserPlainV2Eligible`] (Stage 37).
    RecvV2MetaUserCopy,
    /// Timeout/deadline path requires scheduler interaction.
    TimeoutScheduler,
    /// Message at queue head has cap-transfer or reply-cap flags
    /// (`FLAG_CAP_TRANSFER`, `FLAG_CAP_TRANSFER_PLAIN`, or `FLAG_REPLY_CAP`).
    /// The split path does not perform cap materialization (`take_transfer_envelope`
    /// + cnode lock + `grant_task_to_task_with_rights` / `mint_capability_in_cnode`).
    ///
    /// **Rollback blocker:** for recv-v2 or undersized-buffer failures, the full
    /// path rolls back the already-materialized cap via `rollback_materialized_recv_cap`
    /// (requires re-taking capability lock rank 4 after the failed copy).  Proving
    /// exact rollback semantics on the split path is deferred to a future stage.
    ///
    /// **Message not dequeued:** [`ipc_try_recv_queued_plain_endpoint_only`] rejects
    /// cap-flagged messages before dequeuing; the message stays in queue for the
    /// global-lock full path.
    CapTransfer,
    /// Sender-waiter present but split-unsafe: either the sender-waiter's message
    /// carries cap-transfer / reply-cap flags (requires capability materialization
    /// on refill, which the split path cannot perform), or the sender-waiter queue
    /// is sparse (position 0 is empty but later slots are occupied — indicates a
    /// timed-out sender was removed without compacting the queue).
    ///
    /// Note: a sender-waiter with a **plain** (unflagged) message is now handled
    /// correctly — the dequeued first message is returned as [`RecvOutcome::Delivered`]
    /// with [`RecvSchedulerWakePlan::WakeSender`], applied before writeback.  Only
    /// the cases above (complex message or sparse queue) still produce this fallback.
    SenderWaiterWake,
    /// Empty queue with a blocking policy; requires task block / scheduler.
    Blocking,
    /// Future `recv_shared_v3` request: no live dispatch path exists yet.
    SharedV3HelperOnly,
}

/// Deferred scheduler wake plan produced alongside a delivery.
///
/// Must be applied **after all locks are released**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvSchedulerWakePlan {
    None,
    WakeSender(super::ipc::ThreadId),
}

/// How the received payload should be written after the IPC dequeue.
///
/// The plan carries everything needed for the writeback so it can be applied
/// after `ipc_state_lock` (rank 3) is released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvWritebackPlan {
    /// Kernel task: write `sender_tid` into `ret0`, `raw_len` into `ret1`,
    /// and pack the inline payload words into `arg[3]`/`arg[4]`.
    KernelRegister { sender_tid: usize, raw_len: usize },
    /// User-ASID task: copy the message payload to `ptr` in user space.
    ///
    /// `user_buf_len` is the **capacity** of the caller's buffer (from
    /// `RecvPayloadTarget::UserMemory.len`).  The actual payload to copy is
    /// `delivery.msg.as_slice()`.  The undersized check (`user_buf_len <
    /// payload.len()`) is performed by [`execute_user_asid_plain_writeback`].
    ///
    /// **Stage 36:** live-enabled for narrow plain recv (no meta, no map).
    UserMemory {
        ptr: usize,
        user_buf_len: usize,
        sender_tid: usize,
    },
    /// User-ASID task with recv-v2 metadata: write the 40-byte `IpcRecvMetaV2`
    /// struct to `meta_ptr` first, then copy the payload to `ptr`.
    ///
    /// `meta_ptr`/`meta_len` are the caller's meta buffer (`RecvMetaTarget::V2`).
    /// `user_buf_len` is the payload buffer capacity.  Both copies are performed
    /// by [`execute_user_asid_plain_v2_writeback`] in meta-first order.
    ///
    /// **Stage 37:** live-enabled for user-ASID plain recv-v2 (no map).
    UserMemoryV2 {
        ptr: usize,
        user_buf_len: usize,
        sender_tid: usize,
        meta_ptr: usize,
        meta_len: usize,
    },
}

/// A message delivery produced by the canonical receive engine.
#[derive(Debug, Clone)]
pub struct RecvDelivery {
    /// The dequeued message.
    pub msg: Message,
    /// Frame writeback instructions.
    pub writeback: RecvWritebackPlan,
    /// Deferred sender wake to apply after all locks release.
    pub scheduler: RecvSchedulerWakePlan,
}

/// Outcome of a `try_recv_core_kernel_plain` call.
#[derive(Debug, Clone)]
pub enum RecvOutcome {
    /// A plain message was dequeued; caller applies `delivery.writeback` and
    /// `delivery.scheduler` to the frame and scheduler respectively.
    Delivered(RecvDelivery),
    /// No message available (empty queue); caller handles blocking or
    /// returns `WouldBlock` to userspace.
    WouldBlock,
    /// Timed-out (not produced by `try_recv_core_kernel_plain` directly;
    /// reserved for future timed-recv integration).
    TimedOut,
    /// The core cannot service this request on the split path; caller
    /// **must** use the unchanged global-lock full path.
    FallbackRequired(FallbackReason),
    /// Domain error identical to what the global-lock path would return.
    Error(KernelError),
}

// ─── Core planning ────────────────────────────────────────────────────────────

/// Split-eligibility plan based on the **request shape alone**.
///
/// No IPC state is read here; the actual dequeue may still produce a fallback
/// for reasons determined at dequeue time (empty queue, sender-waiter, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvPlan {
    /// Kernel task (no user ASID), no meta: compatible with the fast
    /// kernel-plain split path.  Execute via [`try_recv_core_kernel_plain`].
    KernelPlainEligible,
    /// **Stage 36:** User-ASID plain recv (no meta, no map_intent).
    /// Compatible with the user-plain split path.
    /// Execute via [`try_recv_core_user_plain`] then
    /// [`execute_user_asid_plain_writeback`].  Copy-failure semantics proven
    /// equivalent to the full path (see `doc/KERNEL_LOCKING.md §54`).
    UserPlainEligible,
    /// **Stage 37:** User-ASID recv-v2 plain recv (V2 meta, no map_intent).
    /// Compatible with the user-plain-v2 split path.
    /// Execute via [`try_recv_core_user_plain_v2`] then
    /// [`execute_user_asid_plain_v2_writeback`].  Meta-first copy ordering and
    /// all three failure modes proven equivalent to the full path (see §55).
    UserPlainV2Eligible,
    /// The request requires the global-lock full path.
    FallbackRequired(FallbackReason),
}

/// Check whether a receive request is eligible for a split path based on its
/// **shape alone**.
///
/// This is a pure function: no kernel state is accessed.  The actual dequeue
/// may still fall back for runtime reasons (empty queue, cap-transfer message
/// at queue head, etc.).
///
/// # Return values
///
/// - [`RecvPlan::KernelPlainEligible`] — kernel task, no meta; use
///   [`try_recv_core_kernel_plain`].
/// - [`RecvPlan::UserPlainEligible`] — user-ASID, no meta, no map_intent;
///   use [`try_recv_core_user_plain`] + [`execute_user_asid_plain_writeback`].
///   Copy-failure semantics proven equivalent to full path (§54).
/// - [`RecvPlan::UserPlainV2Eligible`] — user-ASID, V2 meta, no map_intent;
///   use [`try_recv_core_user_plain_v2`] + [`execute_user_asid_plain_v2_writeback`].
///   Meta-first ordering and all failure modes proven equivalent (§55).
/// - [`RecvPlan::FallbackRequired`] — everything else; use global-lock path.
pub(crate) fn plan_recv_core(request: &RecvRequest) -> RecvPlan {
    // Future v3 is helper-only: never route to live engine.
    if matches!(request.kind, RecvRequestKind::SharedV3Future) {
        return RecvPlan::FallbackRequired(FallbackReason::SharedV3HelperOnly);
    }

    match request.payload_target {
        RecvPayloadTarget::KernelRegister => {
            // Kernel task: V2/V3 meta would require a user-copy for meta struct;
            // no user ASID exists to copy to.
            if matches!(
                request.meta_target,
                RecvMetaTarget::V2 { .. } | RecvMetaTarget::V3Future { .. }
            ) {
                return RecvPlan::FallbackRequired(FallbackReason::RecvV2MetaUserCopy);
            }
            RecvPlan::KernelPlainEligible
        }
        RecvPayloadTarget::UserMemory { .. } => {
            // Mapped receive (map_intent != None) requires page-table operations
            // and region-descriptor copy not yet split-extracted.
            if request.map_intent != RecvMapIntent::None {
                return RecvPlan::FallbackRequired(FallbackReason::UserAsidCopySemantics);
            }
            match request.meta_target {
                // V3Future meta is helper-only; fall back regardless of payload target.
                RecvMetaTarget::V3Future { .. } => {
                    RecvPlan::FallbackRequired(FallbackReason::RecvV2MetaUserCopy)
                }
                // Stage 37: V2 meta + user-ASID + no map_intent → live recv-v2 split path.
                // Meta-first copy ordering and failure modes proven equivalent (§55).
                RecvMetaTarget::V2 { .. } => RecvPlan::UserPlainV2Eligible,
                // Stage 36: no meta + user-ASID + no map_intent → live plain split path (§54).
                RecvMetaTarget::None => RecvPlan::UserPlainEligible,
            }
        }
    }
}

// ─── Core execution ───────────────────────────────────────────────────────────

/// Execute the fast kernel-plain split recv path.
///
/// Attempts to dequeue one plain (unflagged) message from `endpoint` under
/// the IPC domain (`ipc_state_lock`, rank 3) and returns a [`RecvDelivery`]
/// with the kernel-task writeback plan.  All locks are released before any
/// frame write; the caller applies the plan.
///
/// # Preconditions (caller-enforced via [`plan_recv_core`])
///
/// - `request.payload_target == KernelRegister`
/// - `request.meta_target == None`
/// - No capability lock or `ipc_state_lock` held by caller.
///
/// # Lock order
///
/// Acquires and releases `ipc_state_lock` (rank 3) only.
/// Must not be called while holding scheduler (1), task (2), or cap (4) locks.
///
/// # Returns
///
/// - [`RecvOutcome::Delivered`] — plain message dequeued; apply writeback.
/// - [`RecvOutcome::WouldBlock`] — queue empty or another receiver waiter
///   present; caller should block or return `WouldBlock`.
/// - [`RecvOutcome::FallbackRequired`] — ineligible case (sender-waiter wake,
///   cap-transfer message at head, etc.); caller must use global-lock path.
/// - [`RecvOutcome::Error`] — domain error identical to the global-lock path.
pub(crate) fn try_recv_core_kernel_plain(
    kernel: &mut KernelState,
    _request: &RecvRequest,
    endpoint: CapObject,
) -> RecvOutcome {
    let endpoint_idx = match kernel.resolve_endpoint_index(endpoint) {
        Ok(idx) => idx,
        Err(e) => return RecvOutcome::Error(e),
    };

    match kernel.ipc_try_recv_queued_plain_endpoint_only(endpoint_idx) {
        IpcEndpointRecvResult::Received(msg) => {
            kernel.note_endpoint_only_queued_recv_split();
            let sender_tid = match usize::try_from(msg.sender_tid.0) {
                Ok(s) => s,
                Err(_) => return RecvOutcome::Error(KernelError::TaskMissing),
            };
            let raw_len = msg.as_slice().len();
            RecvOutcome::Delivered(RecvDelivery {
                writeback: RecvWritebackPlan::KernelRegister {
                    sender_tid,
                    raw_len,
                },
                scheduler: RecvSchedulerWakePlan::None,
                msg,
            })
        }

        IpcEndpointRecvResult::ReceivedWithSenderWake(msg, wake_tid) => {
            // Stage 38+39: plain sender-waiter refill — deliver original message
            // to receiver and carry deferred wake in RecvSchedulerWakePlan::WakeSender.
            // The caller applies apply_split_sender_wake_plan(wake_tid) BEFORE the
            // writeback, matching the full-path order (§56).
            kernel.note_endpoint_only_queued_recv_split();
            let sender_tid = match usize::try_from(msg.sender_tid.0) {
                Ok(s) => s,
                Err(_) => return RecvOutcome::Error(KernelError::TaskMissing),
            };
            let raw_len = msg.as_slice().len();
            RecvOutcome::Delivered(RecvDelivery {
                writeback: RecvWritebackPlan::KernelRegister {
                    sender_tid,
                    raw_len,
                },
                scheduler: RecvSchedulerWakePlan::WakeSender(wake_tid),
                msg,
            })
        }

        IpcEndpointRecvResult::Ineligible(reason) => match reason {
            IpcEndpointSplitRejectReason::EmptyQueue
            | IpcEndpointSplitRejectReason::ReceiverWaiterPresent => RecvOutcome::WouldBlock,
            IpcEndpointSplitRejectReason::TransferOrReplyCapMessage => {
                RecvOutcome::FallbackRequired(FallbackReason::CapTransfer)
            }
            IpcEndpointSplitRejectReason::SenderWaiterPresent => {
                RecvOutcome::FallbackRequired(FallbackReason::SenderWaiterWake)
            }
            _ => RecvOutcome::WouldBlock,
        },
    }
}

// ─── Stage 36: user-ASID plain recv execution ────────────────────────────────

/// Outcome of a user-ASID plain writeback attempt (`execute_user_asid_plain_writeback`).
///
/// The caller in `syscall.rs` maps each variant to the appropriate frame state
/// and error/fault path — matching the global-lock full path exactly (§54).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvUserWritebackOutcome {
    /// Payload copied successfully.  Caller should call
    /// `frame.set_ok(sender_tid, copied_len, frame.ret2())`.
    Ok,
    /// User buffer capacity (`user_buf_len`) was smaller than the message
    /// payload.  Message consumed.  Caller should return `Err(InvalidArgs)`.
    UndersizedBuffer,
    /// `copy_to_current_user` returned `UserMemoryFault`.  Message consumed.
    /// Caller should call `record_user_fault(kernel, frame, user_ptr, Write)`
    /// and return `Ok(())`.
    CopyFault { user_ptr: usize },
}

/// Execute the fast user-ASID plain split recv path.
///
/// Attempts to dequeue one plain (unflagged) message from `endpoint` under
/// the IPC domain (`ipc_state_lock`, rank 3) and returns a [`RecvDelivery`]
/// with a `UserMemory` writeback plan.  The ipc_state_lock is released before
/// this function returns.  The actual user-space copy is performed by the
/// caller via [`execute_user_asid_plain_writeback`].
///
/// # Preconditions (caller-enforced via [`plan_recv_core`])
///
/// - `request.payload_target == UserMemory { .. }`
/// - `request.meta_target == None`
/// - `request.map_intent == None`
/// - No capability lock or `ipc_state_lock` held by caller.
///
/// # Lock order
///
/// Acquires and releases `ipc_state_lock` (rank 3) only.
/// Must not be called while holding scheduler (1), task (2), or cap (4) locks.
pub(crate) fn try_recv_core_user_plain(
    kernel: &mut KernelState,
    request: &RecvRequest,
    endpoint: CapObject,
) -> RecvOutcome {
    let endpoint_idx = match kernel.resolve_endpoint_index(endpoint) {
        Ok(idx) => idx,
        Err(e) => return RecvOutcome::Error(e),
    };

    match kernel.ipc_try_recv_queued_plain_endpoint_only(endpoint_idx) {
        IpcEndpointRecvResult::Received(msg) => {
            kernel.note_endpoint_only_queued_recv_split();
            let sender_tid = match usize::try_from(msg.sender_tid.0) {
                Ok(s) => s,
                Err(_) => return RecvOutcome::Error(KernelError::TaskMissing),
            };
            let (ptr, user_buf_len) = match request.payload_target {
                RecvPayloadTarget::UserMemory { ptr, len } => (ptr, len),
                // Unreachable: plan_recv_core guarantees UserMemory here.
                _ => unreachable!("try_recv_core_user_plain: non-UserMemory payload_target"),
            };
            RecvOutcome::Delivered(RecvDelivery {
                writeback: RecvWritebackPlan::UserMemory {
                    ptr,
                    user_buf_len,
                    sender_tid,
                },
                scheduler: RecvSchedulerWakePlan::None,
                msg,
            })
        }

        IpcEndpointRecvResult::ReceivedWithSenderWake(msg, wake_tid) => {
            kernel.note_endpoint_only_queued_recv_split();
            let sender_tid = match usize::try_from(msg.sender_tid.0) {
                Ok(s) => s,
                Err(_) => return RecvOutcome::Error(KernelError::TaskMissing),
            };
            let (ptr, user_buf_len) = match request.payload_target {
                RecvPayloadTarget::UserMemory { ptr, len } => (ptr, len),
                _ => unreachable!("try_recv_core_user_plain: ReceivedWithSenderWake: non-UserMemory payload_target"),
            };
            RecvOutcome::Delivered(RecvDelivery {
                writeback: RecvWritebackPlan::UserMemory {
                    ptr,
                    user_buf_len,
                    sender_tid,
                },
                scheduler: RecvSchedulerWakePlan::WakeSender(wake_tid),
                msg,
            })
        }

        IpcEndpointRecvResult::Ineligible(reason) => match reason {
            IpcEndpointSplitRejectReason::EmptyQueue
            | IpcEndpointSplitRejectReason::ReceiverWaiterPresent => RecvOutcome::WouldBlock,
            IpcEndpointSplitRejectReason::TransferOrReplyCapMessage => {
                RecvOutcome::FallbackRequired(FallbackReason::CapTransfer)
            }
            IpcEndpointSplitRejectReason::SenderWaiterPresent => {
                RecvOutcome::FallbackRequired(FallbackReason::SenderWaiterWake)
            }
            _ => RecvOutcome::WouldBlock,
        },
    }
}

/// Perform the user-space copy for a dequeued plain message.
///
/// Called after the ipc_state_lock (rank 3) is released.  The global lock
/// (state spinlock) may still be held by the caller — that is not prohibited
/// by the hard rules, which only forbid holding `ipc_state_lock` or the
/// capability lock during user copies.
///
/// Returns [`RecvUserWritebackOutcome`]; the caller applies frame writes and
/// fault recording based on the outcome.  This matches the global-lock full
/// path semantics exactly (see `doc/KERNEL_LOCKING.md §54`):
///
/// - Undersized buffer → `UndersizedBuffer` (message consumed, same as full path `Err(InvalidArgs)`)
/// - `UserMemoryFault` → `CopyFault` (message consumed, same as full path `record_user_fault + Ok()`)
/// - Success → `Ok` (message delivered to user)
pub(crate) fn execute_user_asid_plain_writeback(
    kernel: &mut KernelState,
    delivery: &RecvDelivery,
) -> RecvUserWritebackOutcome {
    let (ptr, user_buf_len) = match delivery.writeback {
        RecvWritebackPlan::UserMemory {
            ptr, user_buf_len, ..
        } => (ptr, user_buf_len),
        _ => unreachable!("execute_user_asid_plain_writeback: non-UserMemory plan"),
    };
    let payload = delivery.msg.as_slice();
    if user_buf_len < payload.len() {
        return RecvUserWritebackOutcome::UndersizedBuffer;
    }
    match kernel.copy_to_current_user(ptr, payload) {
        Ok(()) => RecvUserWritebackOutcome::Ok,
        Err(KernelError::UserMemoryFault) => RecvUserWritebackOutcome::CopyFault { user_ptr: ptr },
        Err(_) => RecvUserWritebackOutcome::CopyFault { user_ptr: ptr },
    }
}

// ─── Stage 37: user-ASID recv-v2 plain recv execution ────────────────────────

/// Outcome of a user-ASID recv-v2 plain writeback attempt.
///
/// The caller in `syscall.rs` maps each variant to the exact frame state and
/// return value that the global-lock full path produces (§55).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvV2WritebackOutcome {
    /// Meta struct and payload both copied successfully.
    /// Caller should call `frame.set_ok(0, payload_len, frame.ret2())`.
    /// (`ret0 = 0` in recv-v2 mode, matching the full path.)
    Ok,
    /// Meta copy succeeded but the user payload buffer was too small.
    /// Message consumed.  Caller should return `Err(InvalidArgs)`.
    PayloadUndersized,
    /// `copy_to_current_user` returned `UserMemoryFault` for the **meta** buffer.
    /// Message consumed.  Caller should return `Err(SyscallError::PageFault)`,
    /// matching the full-path `return Err(SyscallError::from(copy_err))` (§55).
    MetaCopyFault { meta_ptr: usize },
    /// Meta copy succeeded but `copy_to_current_user` failed for the payload.
    /// Message consumed.  Caller should call `record_user_fault(kernel, frame,
    /// user_ptr, Write)` and return `Ok(())`.
    PayloadCopyFault { user_ptr: usize },
}

/// Execute the fast user-ASID recv-v2 plain split recv path.
///
/// Attempts to dequeue one plain (unflagged) message from `endpoint` under
/// the IPC domain (`ipc_state_lock`, rank 3) and returns a [`RecvDelivery`]
/// with a `UserMemoryV2` writeback plan containing both the meta and payload
/// buffer pointers.  The ipc_state_lock is released before this function returns.
/// The actual copies are performed by the caller via
/// [`execute_user_asid_plain_v2_writeback`].
///
/// # Preconditions (caller-enforced via [`plan_recv_core`])
///
/// - `request.payload_target == UserMemory { .. }`
/// - `request.meta_target == V2 { .. }`
/// - `request.map_intent == None`
/// - No capability lock or `ipc_state_lock` held by caller.
///
/// # Lock order
///
/// Acquires and releases `ipc_state_lock` (rank 3) only.
pub(crate) fn try_recv_core_user_plain_v2(
    kernel: &mut KernelState,
    request: &RecvRequest,
    endpoint: CapObject,
) -> RecvOutcome {
    let endpoint_idx = match kernel.resolve_endpoint_index(endpoint) {
        Ok(idx) => idx,
        Err(e) => return RecvOutcome::Error(e),
    };

    match kernel.ipc_try_recv_queued_plain_endpoint_only(endpoint_idx) {
        IpcEndpointRecvResult::Received(msg) => {
            kernel.note_endpoint_only_queued_recv_split();
            let sender_tid = match usize::try_from(msg.sender_tid.0) {
                Ok(s) => s,
                Err(_) => return RecvOutcome::Error(KernelError::TaskMissing),
            };
            let (ptr, user_buf_len) = match request.payload_target {
                RecvPayloadTarget::UserMemory { ptr, len } => (ptr, len),
                _ => unreachable!("try_recv_core_user_plain_v2: non-UserMemory payload_target"),
            };
            let (meta_ptr, meta_len) = match request.meta_target {
                RecvMetaTarget::V2 { ptr, len } => (ptr, len),
                _ => unreachable!("try_recv_core_user_plain_v2: non-V2 meta_target"),
            };
            RecvOutcome::Delivered(RecvDelivery {
                writeback: RecvWritebackPlan::UserMemoryV2 {
                    ptr,
                    user_buf_len,
                    sender_tid,
                    meta_ptr,
                    meta_len,
                },
                scheduler: RecvSchedulerWakePlan::None,
                msg,
            })
        }

        IpcEndpointRecvResult::ReceivedWithSenderWake(msg, wake_tid) => {
            kernel.note_endpoint_only_queued_recv_split();
            let sender_tid = match usize::try_from(msg.sender_tid.0) {
                Ok(s) => s,
                Err(_) => return RecvOutcome::Error(KernelError::TaskMissing),
            };
            let (ptr, user_buf_len) = match request.payload_target {
                RecvPayloadTarget::UserMemory { ptr, len } => (ptr, len),
                _ => unreachable!("try_recv_core_user_plain_v2: ReceivedWithSenderWake: non-UserMemory payload_target"),
            };
            let (meta_ptr, meta_len) = match request.meta_target {
                RecvMetaTarget::V2 { ptr, len } => (ptr, len),
                _ => unreachable!("try_recv_core_user_plain_v2: ReceivedWithSenderWake: non-V2 meta_target"),
            };
            RecvOutcome::Delivered(RecvDelivery {
                writeback: RecvWritebackPlan::UserMemoryV2 {
                    ptr,
                    user_buf_len,
                    sender_tid,
                    meta_ptr,
                    meta_len,
                },
                scheduler: RecvSchedulerWakePlan::WakeSender(wake_tid),
                msg,
            })
        }

        IpcEndpointRecvResult::Ineligible(reason) => match reason {
            IpcEndpointSplitRejectReason::EmptyQueue
            | IpcEndpointSplitRejectReason::ReceiverWaiterPresent => RecvOutcome::WouldBlock,
            IpcEndpointSplitRejectReason::TransferOrReplyCapMessage => {
                RecvOutcome::FallbackRequired(FallbackReason::CapTransfer)
            }
            IpcEndpointSplitRejectReason::SenderWaiterPresent => {
                RecvOutcome::FallbackRequired(FallbackReason::SenderWaiterWake)
            }
            _ => RecvOutcome::WouldBlock,
        },
    }
}

/// Perform the user-space copies for a dequeued recv-v2 plain message.
///
/// Called after the ipc_state_lock (rank 3) is released.  Copy ordering
/// matches the global-lock full path **exactly** (§55): meta struct is written
/// first, payload second.  All three failure modes are reproduced:
///
/// - Meta `UserMemoryFault` → `MetaCopyFault` (message consumed; caller returns
///   `Err(PageFault)`, matching the full-path `return Err(SyscallError::from(copy_err))`)
/// - Payload undersized (after meta succeeds) → `PayloadUndersized` (message consumed;
///   caller returns `Err(InvalidArgs)`)
/// - Payload `UserMemoryFault` (after meta succeeds) → `PayloadCopyFault` (message
///   consumed; caller calls `record_user_fault + Ok()`)
///
/// For plain messages: `recv_meta_flags = 0`, `recv_local_transfer = None`,
/// no cap rollback on any failure.  `app_payload = msg.as_slice()` (no prefix
/// stripping).  Transfer-cap field in meta = `Message::NO_TRANSFER_CAP`.
pub(crate) fn execute_user_asid_plain_v2_writeback(
    kernel: &mut KernelState,
    delivery: &RecvDelivery,
) -> RecvV2WritebackOutcome {
    let (ptr, user_buf_len, meta_ptr) = match delivery.writeback {
        RecvWritebackPlan::UserMemoryV2 {
            ptr,
            user_buf_len,
            meta_ptr,
            ..
        } => (ptr, user_buf_len, meta_ptr),
        _ => unreachable!("execute_user_asid_plain_v2_writeback: non-UserMemoryV2 plan"),
    };
    let msg = &delivery.msg;
    let app_payload = msg.as_slice();

    // Build the 40-byte IpcRecvMetaV2 struct.
    // Layout and ordering MUST match handle_ipc_recv_result_with_empty_error (§55):
    //   [0..8]   sender as u64 (= msg.sender_tid.0; same value since 64-bit usize)
    //   [8..10]  app_opcode = msg.opcode (no prefix stripping for plain messages)
    //   [10..12] msg.flags
    //   [12..16] app_payload.len() as u32
    //   [16..24] Message::NO_TRANSFER_CAP (plain: recv_local_transfer = None)
    //   [24..32] 0u64 (recv_meta_flags = 0 for plain: no reply/transfer flags)
    //   [32..40] msg.sender_tid.0 (raw sender TID)
    let mut meta = [0u8; META_V2_MIN_LEN];
    meta[0..8].copy_from_slice(&msg.sender_tid.0.to_le_bytes());
    meta[8..10].copy_from_slice(&msg.opcode.to_le_bytes());
    meta[10..12].copy_from_slice(&msg.flags.to_le_bytes());
    meta[12..16].copy_from_slice(&(app_payload.len() as u32).to_le_bytes());
    meta[16..24].copy_from_slice(&Message::NO_TRANSFER_CAP.to_le_bytes());
    meta[24..32].copy_from_slice(&0u64.to_le_bytes());
    meta[32..40].copy_from_slice(&msg.sender_tid.0.to_le_bytes());

    // Copy meta FIRST — matching full-path ordering (§55).
    // On fault: message consumed, caller returns Err(PageFault) (no cap rollback for plain).
    match kernel.copy_to_current_user(meta_ptr, &meta) {
        Ok(()) => {}
        Err(KernelError::UserMemoryFault) => {
            return RecvV2WritebackOutcome::MetaCopyFault { meta_ptr }
        }
        Err(_) => return RecvV2WritebackOutcome::MetaCopyFault { meta_ptr },
    }

    // Undersized buffer check — after meta copy succeeds; message consumed on failure.
    if user_buf_len < app_payload.len() {
        return RecvV2WritebackOutcome::PayloadUndersized;
    }

    // Copy payload SECOND — matching full-path ordering (§55).
    // On fault: message consumed, caller calls record_user_fault + Ok().
    match kernel.copy_to_current_user(ptr, app_payload) {
        Ok(()) => RecvV2WritebackOutcome::Ok,
        Err(KernelError::UserMemoryFault) => {
            RecvV2WritebackOutcome::PayloadCopyFault { user_ptr: ptr }
        }
        Err(_) => RecvV2WritebackOutcome::PayloadCopyFault { user_ptr: ptr },
    }
}

// ─── recv_shared_v3 scaffold ──────────────────────────────────────────────────

/// Future `recv_shared_v3` ABI design scaffold.
///
/// **Helper-only / test-only.** No public syscall number is added in Stage
/// 33+34.  These types model the future v3 ABI for design validation only.
///
/// # Why v3 exists
///
/// Legacy `ipc_recv` and `recv-v2` have accumulated implicit ABI constraints
/// that make it hard to extend cleanly (e.g. exact region length is unknown
/// from frozen legacy mapped receive; cleanup tokens cannot be expressed;
/// object kind/rights/size are not surfaced).  A versioned request/output
/// struct with explicit record-length fields allows future extension without
/// breaking existing callers.
///
/// # Why old recv/v2/mapped remain public compatibility adapters
///
/// ABI stability: userspace already compiled against NR 2/5 syscalls.
/// Switching to v3 requires a coordinated userspace update; v3 therefore starts
/// as an optional new syscall (when eventually added) that old code never sees.
///
/// # Why no public v3 syscall in Stage 33+34
///
/// The canonical receive engine and copy-failure semantics are not yet
/// fully formalized for all user-ASID paths.  Adding a public syscall before
/// the engine is stable would freeze an incomplete ABI.  Stage 36 is the
/// planned public v3 proposal stage.
pub mod recv_shared_v3 {
    /// ABI version number embedded in all v3 request and output records.
    pub const V3_VERSION: u32 = 3;

    /// Minimum byte length of a v3 request record.
    pub const V3_MIN_REQUEST_LEN: u32 = 64;

    /// Minimum byte length of a v3 output record.
    pub const V3_MIN_OUTPUT_LEN: u32 = 80;

    /// Map intent flag: map the transferred region read-only.
    pub const MAP_READ: u32 = 0x1;
    /// Map intent flag: map the transferred region read-write (implies READ).
    pub const MAP_WRITE: u32 = 0x2;

    /// Future `recv_shared_v3` versioned request struct.
    ///
    /// All fields are little-endian when serialised.  `reserved` fields must
    /// be zero on input (validated by [`validate_v3_request`]).
    ///
    /// **Unavailable fields (marked FUTURE):** These fields cannot be
    /// populated from today's kernel state and are documented only to reserve
    /// space in the record layout.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RecvSharedV3Request {
        /// Must equal [`V3_VERSION`].
        pub version: u32,
        /// Total byte length of this record; must be ≥ [`V3_MIN_REQUEST_LEN`].
        pub record_len: u32,
        /// Endpoint capability ID to receive on.
        pub endpoint_cap: u64,
        /// Pointer to user payload buffer.
        pub payload_ptr: u64,
        /// Capacity of user payload buffer in bytes.
        pub payload_len: u64,
        /// Pointer to output metadata record buffer (0 if not needed).
        pub metadata_ptr: u64,
        /// Capacity of output metadata record in bytes.
        pub metadata_len: u64,
        /// Mapping intent flags (`MAP_READ | MAP_WRITE` or 0).
        ///
        /// Must be 0 if `metadata_ptr == 0` (no buffer to receive result).
        pub map_intent: u32,
        /// Behaviour flags (reserved; must be 0).
        pub flags: u32,
        /// Timeout ticks (0 = non-blocking, u64::MAX = block forever).
        pub timeout_ticks: u64,
        /// Reserved; must be zero.
        pub reserved: [u64; 2],
    }

    /// Future `recv_shared_v3` versioned output record.
    ///
    /// Fields marked **FUTURE (unavailable)** cannot be populated from
    /// today's kernel state.  They are present only to reserve record
    /// positions and will be filled in future stages.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RecvSharedV3Output {
        /// Must equal [`V3_VERSION`].
        pub version: u32,
        /// Total byte length of this record; must be ≥ [`V3_MIN_OUTPUT_LEN`].
        pub record_len: u32,
        /// Syscall ABI version (currently 10).
        pub abi_version: u32,
        /// Result status (0 = success).
        pub result_status: u32,
        /// Sender thread ID.
        pub sender_tid: u64,
        /// Received message payload length in bytes.
        pub message_len: u32,
        /// Message flags.
        pub message_flags: u32,
        /// Transferred capability ID in receiver's cnode (`u64::MAX` if none).
        pub transferred_cap: u64,
        /// FUTURE (unavailable): transferred object kind.
        pub object_kind: u32,
        /// FUTURE (unavailable): transferred object generation.
        pub object_generation: u64,
        /// FUTURE (unavailable): effective rights on transferred cap.
        pub effective_rights: u32,
        /// FUTURE (unavailable): exact object size in bytes.
        pub exact_object_size: u64,
        /// Shared-memory region offset (0 if no shared-memory transfer).
        pub region_offset: u64,
        /// FUTURE (unavailable): exact unrounded region length.
        pub exact_region_len: u64,
        /// Mapped virtual base address (0 if no mapping).
        pub mapped_base: u64,
        /// Page-rounded mapped length (0 if no mapping).
        pub page_rounded_mapped_len: u64,
        /// Actual mapping permissions granted.
        pub actual_mapping_perm: u32,
        /// FUTURE (unavailable): cleanup token identity.
        pub cleanup_token: u64,
        /// FUTURE: request ID / descriptor generation for VFS shared I/O.
        pub request_id: u64,
    }

    /// Validation error for v3 request or output records.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RecvSharedV3Error {
        /// `version` field does not equal [`V3_VERSION`].
        BadVersion,
        /// `record_len` is below the minimum.
        ShortRecord,
        /// A `reserved` field was non-zero.
        NonzeroReserved,
        /// `map_intent != 0` but `metadata_ptr == 0`; nowhere to write the
        /// mapping result.
        MetaMapIntentConflict,
        /// `map_intent` contains unrecognised flag bits.
        BadMapIntent,
        /// Output record version or length is invalid.
        BadOutputRecord,
    }

    /// Validate a v3 request record.
    ///
    /// Returns `Ok(())` if the record is well-formed; returns the first
    /// validation error found otherwise.
    pub fn validate_v3_request(req: &RecvSharedV3Request) -> Result<(), RecvSharedV3Error> {
        if req.version != V3_VERSION {
            return Err(RecvSharedV3Error::BadVersion);
        }
        if req.record_len < V3_MIN_REQUEST_LEN {
            return Err(RecvSharedV3Error::ShortRecord);
        }
        // Nonzero reserved fields are a hard reject.
        for &r in &req.reserved {
            if r != 0 {
                return Err(RecvSharedV3Error::NonzeroReserved);
            }
        }
        // flags must be zero in this version.
        if req.flags != 0 {
            return Err(RecvSharedV3Error::NonzeroReserved);
        }
        // map_intent without a metadata buffer is a programming error.
        if req.map_intent != 0 && req.metadata_ptr == 0 {
            return Err(RecvSharedV3Error::MetaMapIntentConflict);
        }
        // Only the defined MAP_READ and MAP_WRITE bits are allowed.
        let known = MAP_READ | MAP_WRITE;
        if req.map_intent & !known != 0 {
            return Err(RecvSharedV3Error::BadMapIntent);
        }
        Ok(())
    }

    /// Validate a v3 output record header.
    pub fn validate_v3_output_record(out: &RecvSharedV3Output) -> Result<(), RecvSharedV3Error> {
        if out.version != V3_VERSION {
            return Err(RecvSharedV3Error::BadVersion);
        }
        if out.record_len < V3_MIN_OUTPUT_LEN {
            return Err(RecvSharedV3Error::BadOutputRecord);
        }
        Ok(())
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::recv_shared_v3::*;
    use super::*;
    use crate::kernel::capabilities::CapId;

    const CAP0: CapId = CapId(1);

    // ── A. Legacy ipc_recv adapter tests ─────────────────────────────────────

    #[test]
    fn recv_core_legacy_adapter_kernel_task_sets_register_target() {
        let req = RecvRequest::from_legacy_ipc_recv(7, CAP0, 0, 0, 0, 0, true);
        assert_eq!(req.kind, RecvRequestKind::LegacyRecv);
        assert_eq!(req.requester_tid, 7);
        assert_eq!(req.recv_cap, CAP0);
        assert_eq!(req.payload_target, RecvPayloadTarget::KernelRegister);
        assert_eq!(req.meta_target, RecvMetaTarget::None);
        assert_eq!(req.blocking, RecvBlockingPolicy::WaitForever);
        assert_eq!(req.transfer, RecvTransferPolicy::LegacyFull);
        assert_eq!(req.map_intent, RecvMapIntent::None);
    }

    #[test]
    fn recv_core_legacy_adapter_user_asid_sets_user_memory_target() {
        let req = RecvRequest::from_legacy_ipc_recv(3, CAP0, 0x4000, 256, 0, 0, false);
        assert_eq!(
            req.payload_target,
            RecvPayloadTarget::UserMemory {
                ptr: 0x4000,
                len: 256
            }
        );
        assert_eq!(req.meta_target, RecvMetaTarget::None);
    }

    #[test]
    fn recv_core_legacy_adapter_v2_meta_detected_when_ptr_and_len_valid() {
        let req = RecvRequest::from_legacy_ipc_recv(1, CAP0, 0x4000, 128, 0x5000, 40, false);
        assert_eq!(
            req.meta_target,
            RecvMetaTarget::V2 {
                ptr: 0x5000,
                len: 40
            }
        );
    }

    #[test]
    fn recv_core_legacy_adapter_no_v2_meta_when_ptr_zero() {
        let req = RecvRequest::from_legacy_ipc_recv(1, CAP0, 0x4000, 128, 0, 40, false);
        assert_eq!(req.meta_target, RecvMetaTarget::None);
    }

    #[test]
    fn recv_core_legacy_adapter_no_v2_meta_when_len_too_small() {
        // meta_len = 39 (< 40) → no V2
        let req = RecvRequest::from_legacy_ipc_recv(1, CAP0, 0x4000, 128, 0x5000, 39, false);
        assert_eq!(req.meta_target, RecvMetaTarget::None);
    }

    // ── B. IpcRecvTimeout adapter tests ──────────────────────────────────────

    #[test]
    fn recv_core_timeout_adapter_zero_ticks_is_nonblocking_probe() {
        let req = RecvRequest::from_ipc_recv_timeout(5, CAP0, 0, 0, 0, None, true);
        assert_eq!(req.kind, RecvRequestKind::NonblockingProbe);
        assert_eq!(req.blocking, RecvBlockingPolicy::NoWait);
    }

    #[test]
    fn recv_core_timeout_adapter_nonzero_ticks_is_timed_recv() {
        let req = RecvRequest::from_ipc_recv_timeout(5, CAP0, 0, 0, 100, Some(9999), true);
        assert_eq!(req.kind, RecvRequestKind::TimedRecv);
        assert_eq!(req.blocking, RecvBlockingPolicy::Deadline(9999));
    }

    #[test]
    fn recv_core_timeout_adapter_nonzero_ticks_without_preread_uses_max() {
        let req = RecvRequest::from_ipc_recv_timeout(5, CAP0, 0, 0, 50, None, true);
        assert_eq!(req.blocking, RecvBlockingPolicy::Deadline(u64::MAX));
    }

    // ── C. recv-v2 adapter tests ──────────────────────────────────────────────

    #[test]
    fn recv_core_recv_v2_adapter_meta_target_populated() {
        let req = RecvRequest::from_recv_v2(2, CAP0, 0x1000, 64, 0x2000, 40, false);
        assert_eq!(
            req.meta_target,
            RecvMetaTarget::V2 {
                ptr: 0x2000,
                len: 40
            }
        );
        assert_eq!(
            req.payload_target,
            RecvPayloadTarget::UserMemory {
                ptr: 0x1000,
                len: 64
            }
        );
    }

    #[test]
    fn recv_core_recv_v2_adapter_small_meta_len_yields_no_meta() {
        let req = RecvRequest::from_recv_v2(2, CAP0, 0x1000, 64, 0x2000, 5, false);
        assert_eq!(req.meta_target, RecvMetaTarget::None);
    }

    // ── D. Copy-failure semantics tests ──────────────────────────────────────
    //
    // These tests document the current copy-failure behaviour without changing
    // it.  See doc/KERNEL_LOCKING.md §52 for the full semantics table.

    #[test]
    fn recv_core_plan_user_asid_plain_is_eligible() {
        // Stage 36: plain user-ASID recv (no meta, no map_intent) is now
        // UserPlainEligible.  Copy-failure semantics proven equivalent to full
        // path; see doc/KERNEL_LOCKING.md §54.
        let req = RecvRequest::from_legacy_ipc_recv(1, CAP0, 0x4000, 128, 0, 0, false);
        assert_eq!(
            plan_recv_core(&req),
            RecvPlan::UserPlainEligible,
            "plain user-ASID recv must be UserPlainEligible (Stage 36)"
        );
    }

    #[test]
    fn recv_core_plan_recv_v2_returns_fallback_meta_copy() {
        // Use is_kernel_task=true so payload_target=KernelRegister bypasses the
        // user-ASID check and the V2 meta check is reached instead.
        let req = RecvRequest::from_recv_v2(1, CAP0, 0, 0, 0x2000, 40, true);
        assert_eq!(
            plan_recv_core(&req),
            RecvPlan::FallbackRequired(FallbackReason::RecvV2MetaUserCopy),
            "recv-v2 with V2 meta target must fall back (meta user-copy)"
        );
    }

    #[test]
    fn recv_core_plan_kernel_task_no_meta_is_eligible() {
        let req = RecvRequest::from_legacy_ipc_recv(0, CAP0, 0, 0, 0, 0, true);
        assert_eq!(
            plan_recv_core(&req),
            RecvPlan::KernelPlainEligible,
            "kernel-task plain recv with no meta must be eligible"
        );
    }

    #[test]
    fn recv_core_plan_kernel_task_with_v2_meta_falls_back() {
        // Even a kernel-task receiver with a v2 meta pointer falls back
        // (the meta would need a user-copy for the receiver's meta buffer).
        let req = RecvRequest::from_recv_v2(0, CAP0, 0, 0, 0x5000, 40, true);
        // meta_target = V2 → fallback
        assert_eq!(
            plan_recv_core(&req),
            RecvPlan::FallbackRequired(FallbackReason::RecvV2MetaUserCopy),
        );
    }

    // ── E. recv_shared_v3 design tests ───────────────────────────────────────

    fn minimal_v3_request() -> RecvSharedV3Request {
        RecvSharedV3Request {
            version: V3_VERSION,
            record_len: V3_MIN_REQUEST_LEN,
            endpoint_cap: 1,
            payload_ptr: 0x1000,
            payload_len: 128,
            metadata_ptr: 0,
            metadata_len: 0,
            map_intent: 0,
            flags: 0,
            timeout_ticks: u64::MAX,
            reserved: [0; 2],
        }
    }

    fn minimal_v3_output() -> RecvSharedV3Output {
        RecvSharedV3Output {
            version: V3_VERSION,
            record_len: V3_MIN_OUTPUT_LEN,
            abi_version: 10,
            result_status: 0,
            sender_tid: 0,
            message_len: 0,
            message_flags: 0,
            transferred_cap: u64::MAX,
            object_kind: 0,
            object_generation: 0,
            effective_rights: 0,
            exact_object_size: 0,
            region_offset: 0,
            exact_region_len: 0,
            mapped_base: 0,
            page_rounded_mapped_len: 0,
            actual_mapping_perm: 0,
            cleanup_token: 0,
            request_id: 0,
        }
    }

    #[test]
    fn recv_shared_v3_valid_request_passes() {
        assert_eq!(validate_v3_request(&minimal_v3_request()), Ok(()));
    }

    #[test]
    fn recv_shared_v3_rejects_bad_version() {
        let mut req = minimal_v3_request();
        req.version = 2;
        assert_eq!(
            validate_v3_request(&req),
            Err(RecvSharedV3Error::BadVersion)
        );
    }

    #[test]
    fn recv_shared_v3_rejects_version_zero() {
        let mut req = minimal_v3_request();
        req.version = 0;
        assert_eq!(
            validate_v3_request(&req),
            Err(RecvSharedV3Error::BadVersion)
        );
    }

    #[test]
    fn recv_shared_v3_rejects_short_record() {
        let mut req = minimal_v3_request();
        req.record_len = V3_MIN_REQUEST_LEN - 1;
        assert_eq!(
            validate_v3_request(&req),
            Err(RecvSharedV3Error::ShortRecord)
        );
    }

    #[test]
    fn recv_shared_v3_rejects_zero_record_len() {
        let mut req = minimal_v3_request();
        req.record_len = 0;
        assert_eq!(
            validate_v3_request(&req),
            Err(RecvSharedV3Error::ShortRecord)
        );
    }

    #[test]
    fn recv_shared_v3_rejects_nonzero_reserved() {
        let mut req = minimal_v3_request();
        req.reserved[0] = 1;
        assert_eq!(
            validate_v3_request(&req),
            Err(RecvSharedV3Error::NonzeroReserved)
        );
    }

    #[test]
    fn recv_shared_v3_rejects_nonzero_flags() {
        let mut req = minimal_v3_request();
        req.flags = 0x1;
        assert_eq!(
            validate_v3_request(&req),
            Err(RecvSharedV3Error::NonzeroReserved)
        );
    }

    #[test]
    fn recv_shared_v3_rejects_map_intent_without_metadata_buffer() {
        // map_intent != 0 but metadata_ptr == 0 → MetaMapIntentConflict
        let mut req = minimal_v3_request();
        req.map_intent = MAP_READ;
        req.metadata_ptr = 0;
        assert_eq!(
            validate_v3_request(&req),
            Err(RecvSharedV3Error::MetaMapIntentConflict)
        );
    }

    #[test]
    fn recv_shared_v3_accepts_read_only_map_intent() {
        let mut req = minimal_v3_request();
        req.map_intent = MAP_READ;
        req.metadata_ptr = 0x8000;
        req.metadata_len = V3_MIN_OUTPUT_LEN as u64;
        assert_eq!(validate_v3_request(&req), Ok(()));
    }

    #[test]
    fn recv_shared_v3_accepts_read_write_map_intent() {
        let mut req = minimal_v3_request();
        req.map_intent = MAP_READ | MAP_WRITE;
        req.metadata_ptr = 0x8000;
        req.metadata_len = V3_MIN_OUTPUT_LEN as u64;
        assert_eq!(validate_v3_request(&req), Ok(()));
    }

    #[test]
    fn recv_shared_v3_rejects_unknown_map_intent_bits() {
        let mut req = minimal_v3_request();
        req.map_intent = 0x8; // unknown bit
        req.metadata_ptr = 0x8000;
        assert_eq!(
            validate_v3_request(&req),
            Err(RecvSharedV3Error::BadMapIntent)
        );
    }

    #[test]
    fn recv_shared_v3_output_record_valid_passes() {
        assert_eq!(validate_v3_output_record(&minimal_v3_output()), Ok(()));
    }

    #[test]
    fn recv_shared_v3_output_record_rejects_bad_version() {
        let mut out = minimal_v3_output();
        out.version = 1;
        assert_eq!(
            validate_v3_output_record(&out),
            Err(RecvSharedV3Error::BadVersion)
        );
    }

    #[test]
    fn recv_shared_v3_output_record_rejects_short_len() {
        let mut out = minimal_v3_output();
        out.record_len = V3_MIN_OUTPUT_LEN - 1;
        assert_eq!(
            validate_v3_output_record(&out),
            Err(RecvSharedV3Error::BadOutputRecord)
        );
    }

    #[test]
    fn recv_shared_v3_future_adapter_helper_only_falls_back() {
        let req = RecvRequest::future_shared_v3(
            1,
            CAP0,
            0x1000,
            64,
            0x2000,
            V3_MIN_OUTPUT_LEN as usize,
            RecvMapIntent::ReadOnly,
            Some(1000),
        );
        assert_eq!(req.kind, RecvRequestKind::SharedV3Future);
        assert_eq!(req.blocking, RecvBlockingPolicy::Deadline(1000));
        assert_eq!(req.map_intent, RecvMapIntent::ReadOnly);
        // v3 is always FallbackRequired
        assert_eq!(
            plan_recv_core(&req),
            RecvPlan::FallbackRequired(FallbackReason::SharedV3HelperOnly),
        );
    }

    #[test]
    fn recv_shared_v3_future_adapter_nowait() {
        let req = RecvRequest::future_shared_v3(1, CAP0, 0, 0, 0, 0, RecvMapIntent::None, Some(0));
        assert_eq!(req.blocking, RecvBlockingPolicy::NoWait);
    }

    #[test]
    fn recv_shared_v3_future_adapter_wait_forever() {
        let req = RecvRequest::future_shared_v3(1, CAP0, 0, 0, 0, 0, RecvMapIntent::None, None);
        assert_eq!(req.blocking, RecvBlockingPolicy::WaitForever);
    }

    // ── plan_recv_core exhaustive coverage ───────────────────────────────────

    #[test]
    fn plan_recv_core_legacy_mapped_recv_falls_back_user_asid() {
        let req =
            RecvRequest::from_legacy_mapped_recv(1, CAP0, 0x4000, 128, RecvMapIntent::ReadOnly);
        assert_eq!(
            plan_recv_core(&req),
            RecvPlan::FallbackRequired(FallbackReason::UserAsidCopySemantics),
        );
    }

    #[test]
    fn plan_recv_core_timed_recv_kernel_task_eligible() {
        // Timed recv with kernel-task target and no meta is shape-eligible.
        // (The timeout is handled at the caller level; the shape here only
        //  says "no user-copy needed".)
        let req = RecvRequest::from_ipc_recv_timeout(0, CAP0, 0, 0, 0, None, true);
        assert_eq!(plan_recv_core(&req), RecvPlan::KernelPlainEligible);
    }
}
