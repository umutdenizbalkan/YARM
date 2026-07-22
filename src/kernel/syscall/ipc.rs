// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! IPC syscall handlers — implementation module only.
//!
//! **Dispatch ownership:** `syscall.rs` owns the syscall-number dispatch table and calls
//! into this module through thin shims. This module must never acquire dispatch ownership,
//! decode syscall numbers, or introduce a second dispatch layer.
//!
//! **Boundary invariants (must not change without a dedicated audit stage):**
//! - IPC ABI: syscall argument positions, return-register encoding, and error codes are
//!   frozen in `syscall.rs`. Constants live there; do not duplicate or redefine them here.
//! - Cap-slot materialization and revocation ordering: `materialize_received_message_cap`
//!   and `complete_blocked_recv_for_waiter` remain in `syscall.rs` as `pub(crate)`.
//!   They must not be moved here without a separate lock-ordering audit.
//! - Reply-cap one-shot semantics: `ipc_reply` consumes the reply-cap slot atomically.
//!   Nothing in this module may re-order that consumption relative to payload delivery.
//! - Shared-memory mapping rights: attenuation and rights checks happen before
//!   `map_shared_region_into_receiver`. Do not reorder without a rights-audit stage.
//! - User-memory copy ordering: `copy_from_current_user` / `copy_to_current_user` calls
//!   must only appear inside the five `pub(super)` handlers, not in private helpers.
//! - Lock ordering: private helpers must not acquire `ipc_state_lock` directly; they
//!   call `KernelState` methods that manage lock rank internally.

use super::{
    IPC_RECV_META_V2_ENCODED_LEN, OPCODE_INLINE, OPCODE_SHARED_MEM, SYSCALL_ARG_CAP,
    SYSCALL_ARG_INLINE_PAYLOAD0, SYSCALL_ARG_INLINE_PAYLOAD1, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR,
    SYSCALL_ARG_TRANSFER_CAP, SYSCALL_NO_TRANSFER_CAP, SYSCALL_RECV_MAP_INTENT_READ,
    SYSCALL_RECV_MAP_INTENT_WRITE, SYSCALL_RECV_META_REPLY_CAP, SYSCALL_RECV_META_TRANSFERRED_CAP,
    SyscallError, clear_blocked_recv_state, complete_blocked_recv_for_waiter,
    current_task_has_user_asid, current_tid, decode_ipc_send_timeout_ticks,
    encode_transfer_cap_ret, materialize_received_message_cap, record_user_fault,
    sender_tid_to_ret, should_strip_inline_opcode_prefix, transfer_cap_arg,
    try_endpoint_split_recv, validate_endpoint_right, validate_user_region,
};
use crate::kernel::boot::{
    IpcEndpointSendResult, IpcSchedulerPlan, KernelError, KernelState, TransferSharedRegion,
};
use crate::kernel::capabilities::{CapId, CapObject, CapRights};
use crate::kernel::ipc::{
    IPC_REGISTER_BYTES, Message, SharedMemoryRegion, pack_register_payload, unpack_register_payload,
};
use crate::kernel::task::{BlockedRecvState, RecvAbiVariant};
use crate::kernel::trap::FaultAccess;
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::{PAGE_SIZE, PageFlags, VirtAddr};

/// Stage 198D-S — DIRECT-ONLY REPLY CAPS (authoritative policy).
///
/// Reply capabilities are special: direct-delivery only, one-shot, and NEVER stored
/// in an endpoint message queue. Queued reply-cap transfer is not part of YARM's
/// supported capability model (the Stage 198D2B live queued class is cancelled). This
/// constant is the single authoritative switch: it is `false`, and both the IpcSend
/// send-side refusal (`handle_ipc_send`) and the recv-side fail-closed guard depend on
/// reply caps having no queued path. Flipping it would re-enable queueing — which is a
/// deliberate policy reversal, not an incidental change.
pub(crate) const REPLY_CAP_QUEUEING_SUPPORTED: bool = false;

fn transfer_flag_bits(transfer_cap: Option<CapId>) -> u16 {
    if transfer_cap.is_some() {
        Message::FLAG_CAP_TRANSFER
    } else {
        0
    }
}

fn validate_transfer_cap(kernel: &KernelState, cap: CapId) -> Result<(), SyscallError> {
    if kernel
        .capability_service()
        .resolve_current_task_capability(cap)
        .is_none()
    {
        return Err(SyscallError::InvalidCapability);
    }
    Ok(())
}

fn log_supervisor_fault_recv_cap_if_applicable(
    kernel: &KernelState,
    recv_tid: u64,
    cap: CapId,
    endpoint: CapObject,
) {
    let CapObject::Endpoint { index, generation } = endpoint else {
        return;
    };
    let is_supervisor_fault_endpoint = kernel.with_fault_state(|faults| {
        faults.fault_handler_endpoint == Some(index) || faults.supervisor_endpoint == Some(index)
    });
    if recv_tid == 2 && is_supervisor_fault_endpoint {
        crate::yarm_log!(
            "SUPERVISOR_FAULT_RECV_CAP cap={} endpoint={} generation={}",
            cap.0,
            index,
            generation
        );
    }
}

fn validate_shared_mem_transfer_rights(
    capability: &crate::kernel::capabilities::Capability,
) -> Result<(), SyscallError> {
    if !capability.has_right(CapRights::READ) || !capability.has_right(CapRights::MAP) {
        return Err(SyscallError::MissingRight);
    }
    Ok(())
}

fn stash_transfer_handle(
    kernel: &mut KernelState,
    transfer_cap: Option<CapId>,
    endpoint: CapObject,
    shared_region: Option<TransferSharedRegion>,
) -> Result<(Option<u64>, Option<crate::kernel::ipc::ThreadId>), SyscallError> {
    let Some(source_cap_id) = transfer_cap else {
        return Ok((None, None));
    };
    let sender_tid = current_tid(kernel)?;
    let _ = kernel
        .resolve_capability_for_task(sender_tid, source_cap_id)
        .map_err(SyscallError::from)?;
    let receiver_tid = kernel.endpoint_waiter_tid(endpoint);
    Ok((
        Some(
            kernel
                .stash_transfer_envelope(
                    crate::kernel::ipc::ThreadId(sender_tid),
                    source_cap_id,
                    endpoint,
                    receiver_tid,
                    shared_region,
                )
                .ok_or(SyscallError::QueueFull)?,
        ),
        receiver_tid,
    ))
}

fn inline_payload_from_frame(
    frame: &TrapFrame,
    len: usize,
) -> Result<[u8; Message::MAX_PAYLOAD], SyscallError> {
    if len > IPC_REGISTER_BYTES || len > Message::MAX_PAYLOAD {
        return Err(SyscallError::InvalidArgs);
    }
    let words = [
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
    ];
    let regs = unpack_register_payload(words, len).ok_or(SyscallError::InvalidArgs)?;
    let mut payload = [0u8; Message::MAX_PAYLOAD];
    payload[..len].copy_from_slice(&regs[..len]);
    Ok(payload)
}

fn map_shared_region_into_receiver(
    kernel: &mut KernelState,
    receiver_mem_cap: CapId,
    requested_va: usize,
    region_len: usize,
    map_flags: PageFlags,
) -> Result<(usize, usize), SyscallError> {
    if requested_va == 0 || region_len == 0 || !requested_va.is_multiple_of(PAGE_SIZE) {
        return Err(SyscallError::InvalidArgs);
    }
    let mapped_len = super::round_up_page(region_len)?;
    let mut va = requested_va;
    let end = requested_va
        .checked_add(mapped_len)
        .ok_or(SyscallError::InvalidArgs)?;
    // Stage 7: resolve ASID once plan-first (rank-2 task read before vm/memory mutation).
    // The caller (handle_ipc_recv_result_with_empty_error) already confirmed
    // current_task_has_user_asid, so task_asid returns Some here.
    let tid = kernel.current_tid().ok_or(SyscallError::Internal)?;
    let asid = kernel
        .task_asid(tid)
        .ok_or(SyscallError::from(KernelError::UserMemoryFault))?;
    while va < end {
        if let Err(err) = kernel.map_user_page_in_asid_with_caps(
            asid,
            receiver_mem_cap,
            VirtAddr(va as u64),
            map_flags,
        ) {
            // Stage 7: two-phase rollback — reclaim only after shootdown wait/fast path.
            let mut rollback = requested_va;
            while rollback < va {
                if let Ok(Some(plan)) = kernel.unmap_page_phase1(asid, VirtAddr(rollback as u64)) {
                    let _ = kernel.execute_tlb_shootdown_wait_plan(plan);
                }
                rollback += PAGE_SIZE;
            }
            return Err(SyscallError::from(err));
        }
        va += PAGE_SIZE;
    }
    Ok((requested_va, mapped_len))
}

fn revoke_current_transfer_cap_best_effort(kernel: &mut KernelState, transfer_cap: CapId) {
    if let Some(cnode) = kernel.current_task_cnode() {
        let _ = kernel.revoke_capability_in_cnode(cnode, transfer_cap);
    }
}

fn attenuate_transfer_cap_for_recv_intent(
    kernel: &mut KernelState,
    transfer_cap: CapId,
    allow_write: bool,
) -> Result<CapId, SyscallError> {
    if allow_write {
        return Ok(transfer_cap);
    }
    let capability = kernel
        .capability_service()
        .resolve_current_task_capability(transfer_cap)
        .ok_or(SyscallError::InvalidCapability)?;
    let desired = CapRights::READ | CapRights::MAP;
    if capability.rights().contains(desired) && !capability.rights().contains(CapRights::WRITE) {
        return Ok(transfer_cap);
    }
    let attenuated_rights = capability.rights().intersect(desired);
    let derived = kernel
        .mint_capability_for_current_context(crate::kernel::capabilities::Capability::new(
            capability.object,
            attenuated_rights,
        ))
        .map_err(SyscallError::from)?;
    revoke_current_transfer_cap_best_effort(kernel, transfer_cap);
    Ok(derived)
}

fn recv_shared_mem_map_intent_flags(frame: &TrapFrame) -> Result<PageFlags, SyscallError> {
    let raw = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1);
    if raw == 0 {
        return Ok(PageFlags {
            read: true,
            write: true,
            execute: false,
            user: true,
            cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
        });
    }
    let unknown = raw & !(SYSCALL_RECV_MAP_INTENT_READ | SYSCALL_RECV_MAP_INTENT_WRITE);
    if unknown != 0 || (raw & SYSCALL_RECV_MAP_INTENT_READ) == 0 {
        return Err(SyscallError::InvalidArgs);
    }
    Ok(PageFlags {
        read: true,
        write: (raw & SYSCALL_RECV_MAP_INTENT_WRITE) != 0,
        execute: false,
        user: true,
        cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
    })
}

pub(super) fn handle_ipc_send(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    validate_endpoint_right(kernel, cap, CapRights::SEND)?;
    let endpoint = kernel
        .capability_service()
        .resolve_current_task_capability(cap)
        .ok_or(SyscallError::InvalidCapability)?
        .object;
    let user_ptr_or_offset = frame.arg(SYSCALL_ARG_PTR);
    let len = frame.arg(SYSCALL_ARG_LEN);
    let transfer_cap = transfer_cap_arg(kernel, frame)?;
    if let Some(c) = transfer_cap {
        validate_transfer_cap(kernel, c)?;
    }
    let sender_tid = current_tid(kernel)?;

    // Stage 198D-S — DIRECT-ONLY REPLY CAPS. Reply capabilities are special: they are
    // direct-delivery only, one-shot, and are NEVER stored in an endpoint message
    // queue. If the source object is a Reply and there is NO compatible (recv-v2
    // blocked) receiver ready to take the accepted Stage 198C direct hand-off, refuse
    // the send up front — BEFORE any TransferEnvelope is stashed or any Message is
    // built — so no queued reply-cap state can ever be created. Ordinary caps and
    // plain messages are unaffected; a Reply hand-off to an already-blocked receiver
    // keeps the accepted Stage 198C direct path below. The canonical WouldBlock error
    // is returned (no new ABI/error code): the operation cannot complete without a
    // ready receiver, and reply caps have no queued fallback.
    if !REPLY_CAP_QUEUEING_SUPPORTED
        && let Some(tc) = transfer_cap
        && matches!(
            kernel
                .capability_service()
                .resolve_current_task_capability(tc)
                .map(|c| c.object),
            Some(CapObject::Reply { .. })
        )
    {
        let direct_receiver = kernel
            .endpoint_waiter_tid(endpoint)
            .filter(|rt| kernel.is_task_recv_v2_blocked(rt.0));
        if direct_receiver.is_none() {
            crate::yarm_log!(
                "IPC_SEND_REPLY_CAP_DIRECT_ONLY tid={} reason=no_blocked_receiver",
                sender_tid
            );
            return Err(SyscallError::WouldBlock);
        }
    }

    let sender_has_user_asid = current_task_has_user_asid(kernel)?;
    let send_timeout_ticks = if sender_has_user_asid || len == 0 {
        decode_ipc_send_timeout_ticks(frame)
    } else {
        0
    };

    let mut stash_bound_receiver_tid: Option<crate::kernel::ipc::ThreadId> = None;
    let msg_result = if sender_has_user_asid {
        if len > Message::MAX_PAYLOAD {
            let grant_cap = transfer_cap.ok_or(SyscallError::InvalidArgs)?;
            let grant = kernel
                .capability_service()
                .resolve_current_task_capability(grant_cap)
                .ok_or(SyscallError::InvalidCapability)?;
            match grant.object {
                CapObject::MemoryObject { .. } | CapObject::DmaRegion { .. } => {}
                _ => return Err(SyscallError::WrongObject),
            }
            validate_shared_mem_transfer_rights(&grant)?;
            validate_user_region(user_ptr_or_offset as u64, len as u64)?;
            let region = SharedMemoryRegion {
                offset: user_ptr_or_offset as u64,
                len: len as u64,
            };
            let (transfer_handle, bound_tid) = stash_transfer_handle(
                kernel,
                transfer_cap,
                endpoint,
                Some(TransferSharedRegion {
                    offset: region.offset,
                    len: region.len,
                }),
            )?;
            stash_bound_receiver_tid = bound_tid;
            Message::with_header(
                sender_tid,
                OPCODE_SHARED_MEM,
                Message::FLAG_CAP_TRANSFER,
                transfer_handle,
                &region.encode(),
            )
            .map_err(|_| SyscallError::InvalidArgs)
        } else {
            let payload = match kernel.copy_from_current_user(user_ptr_or_offset, len) {
                Ok(payload) => payload,
                Err(KernelError::UserMemoryFault) => {
                    record_user_fault(kernel, frame, user_ptr_or_offset, FaultAccess::Read);
                    return Ok(());
                }
                Err(other) => return Err(SyscallError::from(other)),
            };

            let (transfer_handle, bound_tid) =
                stash_transfer_handle(kernel, transfer_cap, endpoint, None)?;
            stash_bound_receiver_tid = bound_tid;
            Message::with_header(
                sender_tid,
                OPCODE_INLINE,
                transfer_flag_bits(transfer_cap),
                transfer_handle,
                &payload[..len],
            )
            .map_err(|_| SyscallError::InvalidArgs)
        }
    } else {
        if len > Message::MAX_PAYLOAD {
            return Err(SyscallError::InvalidArgs);
        }
        if len > IPC_REGISTER_BYTES {
            let grant_cap = transfer_cap.ok_or(SyscallError::InvalidArgs)?;
            let grant = kernel
                .capability_service()
                .resolve_current_task_capability(grant_cap)
                .ok_or(SyscallError::InvalidCapability)?;
            match grant.object {
                CapObject::MemoryObject { .. } | CapObject::DmaRegion { .. } => {}
                _ => return Err(SyscallError::WrongObject),
            }
            validate_shared_mem_transfer_rights(&grant)?;
            let region = SharedMemoryRegion {
                offset: user_ptr_or_offset as u64,
                len: len as u64,
            };
            let (transfer_handle, bound_tid) = stash_transfer_handle(
                kernel,
                transfer_cap,
                endpoint,
                Some(TransferSharedRegion {
                    offset: region.offset,
                    len: region.len,
                }),
            )?;
            stash_bound_receiver_tid = bound_tid;
            Message::with_header(
                sender_tid,
                OPCODE_SHARED_MEM,
                Message::FLAG_CAP_TRANSFER,
                transfer_handle,
                &region.encode(),
            )
            .map_err(|_| SyscallError::InvalidArgs)
        } else {
            let payload = inline_payload_from_frame(frame, len)?;
            let (transfer_handle, bound_tid) =
                stash_transfer_handle(kernel, transfer_cap, endpoint, None)?;
            stash_bound_receiver_tid = bound_tid;
            Message::with_header(
                sender_tid,
                OPCODE_INLINE,
                transfer_flag_bits(transfer_cap),
                transfer_handle,
                &payload[..len],
            )
            .map_err(|_| SyscallError::InvalidArgs)
        }
    };
    let msg = match msg_result {
        Ok(msg) => msg,
        Err(err) => return Err(err),
    };

    // VALIDATION: LIVE_OFF_TRAP
    // VALIDATION: SPLIT_FAST_PATH_ONLY
    // Stage 4E / Stage 4F / Stage 4K / Stage 4O: split-send fast path off the
    // trap-entry seam. Cases this match cannot service set
    // `split_send_result = None` and the caller falls back to the global-lock
    // `kernel.ipc_send(...)` / `ipc_send_with_deadline(...)` paths below.
    // See doc/KERNEL_UNLOCKING.md
    let (split_send_result, split_scheduler_plan) = match endpoint {
        CapObject::Endpoint { .. } => {
            let endpoint_idx = kernel
                .resolve_endpoint_index(endpoint)
                .map_err(SyscallError::from)?;
            // Stage 193E (BROAD-IPC DECOMPOSITION): route the endpoint-only enqueue through
            // the plain no-waiter enqueue boundary split. For a PLAIN message with no blocked
            // receiver it emits the enqueue boundary markers + fires the IpcSendPlainEnqueue
            // retirement; non-plain / waiter-present / ineligible cases are byte-identical to
            // the unchanged Stage 4E path (same IpcEndpointSendResult).
            match kernel.ipc_try_send_enqueue_boundary_split_plain(endpoint_idx, msg) {
                IpcEndpointSendResult::Enqueued => {
                    kernel.note_endpoint_only_queued_send_split();
                    // Stage 4E now accepts FLAG_CAP_TRANSFER / FLAG_CAP_TRANSFER_PLAIN
                    // (cap already stashed in transfer-envelope table by stash_transfer_handle).
                    if (msg.flags & (Message::FLAG_CAP_TRANSFER | Message::FLAG_CAP_TRANSFER_PLAIN))
                        != 0
                    {
                        kernel.note_cap_transfer_stage4e_enqueued();
                    }
                    (Some(Ok(())), IpcSchedulerPlan::None)
                }
                IpcEndpointSendResult::EnqueuedWakeReceiver(_) => {
                    unreachable!("Stage 4E never returns EnqueuedWakeReceiver")
                }
                IpcEndpointSendResult::ReceiverWaiterFound(receiver) => {
                    // Stage 4F: ipc_try_send_queued_plain_endpoint_only found a plain
                    // receiver waiter with no sender waiters. The COMPLETE identity (tid + ASID)
                    // came from an ipc_state_lock read — no unlocked waiter array access needed.
                    // Check recv-v2 under task_state_lock (rank 3) BEFORE
                    // ipc_state_lock (rank 4) — required by lock ordering.
                    let receiver_tid = receiver.tid;
                    let is_recv_v2 = kernel.is_task_recv_v2_blocked(receiver_tid.0);
                    if !is_recv_v2 {
                        // Stage 4F: non-recv-v2 receiver. Cap-transfer messages return
                        // Ineligible here (split_unsafe_flags check in
                        // ipc_try_send_to_plain_receiver_endpoint_only). The plain-send re-verifies
                        // the FULL identity before clearing the waiter (never numeric TID alone).
                        match kernel.ipc_try_send_to_plain_receiver_endpoint_only(
                            endpoint_idx,
                            receiver,
                            msg,
                        ) {
                            IpcEndpointSendResult::EnqueuedWakeReceiver(recv_tid) => {
                                kernel.note_endpoint_only_queued_send_split();
                                (Some(Ok(())), IpcSchedulerPlan::WakeReceiver(recv_tid))
                            }
                            _ => (None, IpcSchedulerPlan::None),
                        }
                    } else {
                        // Stage 193A/193C/193D (BROAD-IPC DECOMPOSITION): try the IpcSend
                        // boundary splits in order — plain → reply-cap object → ordinary cap
                        // (see `try_ipc_send_boundary_split_any_pub`). On Ok(true) a producer
                        // snapshotted by value under the broad borrow (consuming any transfer
                        // envelope ONCE); the trap-entry drain does the user copy / cap
                        // materialize + wake AFTER the broad borrow drops (no ipc_state_lock
                        // across the copy/mint), so NO in-lock wake plan here (the drain wakes
                        // exactly once). On Ok(false) (shared-region / no drainer) nothing was
                        // consumed → the legacy in-broad-lock path below runs. On Err a
                        // producer consumed state then hit a real Phase-A fault → map to
                        // UserMemoryFault (do NOT re-run the legacy delivery).
                        match kernel.try_ipc_send_boundary_split_any_pub(
                            receiver_tid.0,
                            endpoint_idx,
                            &msg,
                        ) {
                            Ok(true) => (Some(Ok(())), IpcSchedulerPlan::None),
                            Err(_e) => (
                                Some(Err(KernelError::UserMemoryFault)),
                                IpcSchedulerPlan::None,
                            ),
                            Ok(false) => {
                                // Stage 198E3: shared-region DIRECT live path — gated behind the
                                // oracle-proof knob. Under the knob (and a drainer), the producer
                                // snapshots the accepted post-lock transaction (envelope consume +
                                // pin transfer) by value; the trap-entry drain runs
                                // `shared_region_execute` after the broad borrow drops (map, meta
                                // copy, revalidate, single wake) — NO map/copy under the broad
                                // borrow. Off the knob → Ok(false) → the legacy path below runs
                                // (unchanged normal boot). Err → a real Phase-A fault.
                                match crate::kernel::syscall::produce_blocked_waiter_shared_region_delivery(
                                    kernel,
                                    receiver_tid.0,
                                    endpoint_idx,
                                    &msg,
                                ) {
                                    // Producer stashed the post-work (drain completes delivery +
                                    // slot-clear + single wake) — no in-lock wake plan here.
                                    Ok(true) => (Some(Ok(())), IpcSchedulerPlan::None),
                                    // Stage 198E3C1C: the oracle-armed pre-ack FAIL-CLOSED decline
                                    // returns the canonical retryable WouldBlock (NOT a fault, NOT
                                    // legacy delivery): no blocked state was taken, nothing mapped/
                                    // minted/queued/woken; the outer path releases the transfer
                                    // envelope so the source cap is preserved and the parent retries.
                                    Err(SyscallError::WouldBlock) => (
                                        Some(Err(KernelError::WouldBlock)),
                                        IpcSchedulerPlan::None,
                                    ),
                                    // A real Phase-A fault — the outer error path releases any
                                    // still-stashed envelope (same disposition as the legacy arm).
                                    Err(_e) => (
                                        Some(Err(KernelError::UserMemoryFault)),
                                        IpcSchedulerPlan::None,
                                    ),
                                    Ok(false) => {
                                // Stage 4K/4O legacy: recv-v2 blocked receiver — deliver
                                // directly outside ipc_state_lock (still under the broad
                                // borrow). Handles the shared-region variant all boundary
                                // slices decline. Return Some(Err) on failure (not ?) so the
                                // outer error path can release the transfer envelope.
                                match complete_blocked_recv_for_waiter(kernel, receiver_tid.0, &msg)
                                {
                                    Ok(()) => {
                                        kernel.ipc_clear_plain_receiver_waiter_only(
                                            endpoint_idx,
                                            receiver_tid,
                                        );
                                        kernel.note_split_recv_v2_delivery();
                                        if transfer_cap.is_some() {
                                            kernel.note_cap_transfer_recv_v2_delivery();
                                        }
                                        (Some(Ok(())), IpcSchedulerPlan::WakeReceiver(receiver_tid))
                                    }
                                    Err(_err) => (
                                        Some(Err(KernelError::UserMemoryFault)),
                                        IpcSchedulerPlan::None,
                                    ),
                                }
                                    }
                                }
                            }
                        }
                    }
                }
                IpcEndpointSendResult::Ineligible(_) => (None, IpcSchedulerPlan::None),
            }
        }
        _ => (None, IpcSchedulerPlan::None),
    };
    let send_result = if let Some(send_result) = split_send_result {
        send_result
    } else if send_timeout_ticks == 0 {
        kernel.ipc_send(cap, msg)
    } else {
        kernel.ipc_send_with_deadline(cap, msg, send_timeout_ticks)
    };
    if let Err(err) = send_result {
        if let Some(handle) = msg.transferred_cap().map(|c| c.0) {
            // Use the receiver TID that was bound at stash time. Passing sender_tid
            // would fail the bound-receiver check inside take_transfer_envelope when
            // endpoint_waiter_tid returned Some(waiter_tid) at stash time.
            let cleanup_tid =
                stash_bound_receiver_tid.unwrap_or(crate::kernel::ipc::ThreadId(sender_tid));
            let _ = kernel.take_transfer_envelope(handle, endpoint, cleanup_tid);
        }
        if err == KernelError::WouldBlock && send_timeout_ticks != 0 {
            let timed_out = kernel
                .consume_ipc_timeout_fired_for_tid(sender_tid)
                .map_err(SyscallError::from)?;
            if timed_out {
                return Err(SyscallError::TimedOut);
            }
            let still_blocked = matches!(
                kernel.task_status(sender_tid),
                Some(crate::kernel::task::TaskStatus::Blocked(
                    crate::kernel::task::WaitReason::EndpointSend(_)
                ))
            );
            if !still_blocked {
                frame.set_ok(0, 0, 0);
                encode_transfer_cap_ret(frame, None)?;
                return Ok(());
            }
        }
        return Err(SyscallError::from(err));
    }
    // Stage 4F: apply deferred receiver-wake plan outside ipc_state_lock.
    if let IpcSchedulerPlan::WakeReceiver(recv_tid) = split_scheduler_plan {
        let _ = kernel.apply_split_receiver_wake_plan(recv_tid);
    }
    frame.set_ok(0, 0, 0);
    encode_transfer_cap_ret(frame, None)?;
    Ok(())
}

pub(super) fn handle_ipc_recv(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    use crate::kernel::recv_core::{RecvMetaTarget, RecvRequest};

    let cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let recv_tid = kernel.current_tid().unwrap_or(0);
    crate::yarm_log!("IPC_RECV_ENTER tid={} cap={}", recv_tid, cap.0);

    // Stage 35: build canonical request for decode/planning; this is the same
    // logic the split path uses, now also exercised on the full-path entry.
    let is_kernel_task = matches!(current_task_has_user_asid(kernel), Ok(false));
    let request = RecvRequest::from_legacy_ipc_recv(
        recv_tid as u64,
        cap,
        frame.arg(SYSCALL_ARG_PTR),
        frame.arg(SYSCALL_ARG_LEN),
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
        is_kernel_task,
    );
    crate::yarm_log!(
        "YARM_RECV_CORE_ADAPTER kind=legacy_full_path is_kernel_task={}",
        is_kernel_task
    );

    if let Err(e) = validate_endpoint_right(kernel, cap, CapRights::RECEIVE) {
        clear_blocked_recv_state(kernel, recv_tid, "error");
        crate::yarm_log!(
            "IPC_RECV_CAP_LOOKUP_FAIL tid={} cap={} reason={:?}",
            recv_tid,
            cap.0,
            e
        );
        return Err(e);
    }
    let endpoint_cap = kernel
        .current_task_cnode()
        .and_then(|cnode| kernel.capability_for_cnode_local(cnode, cap))
        .and_then(|capability| {
            kernel
                .capability_object_live(capability.object)
                .map(|_| capability)
        });
    let Some(endpoint_cap) = endpoint_cap else {
        clear_blocked_recv_state(kernel, recv_tid, "error");
        crate::yarm_log!(
            "IPC_RECV_INVALID_CAP_SOURCE reason=post_validate_endpoint_lookup tid={} cap={} endpoint={}",
            recv_tid,
            cap.0,
            u64::MAX
        );
        return Err(SyscallError::InvalidCapability);
    };
    let endpoint = endpoint_cap.object;
    crate::yarm_log!(
        "IPC_RECV_AFTER_CAP_OK tid={} cap={} endpoint={:?}",
        recv_tid,
        cap.0,
        endpoint
    );
    log_supervisor_fault_recv_cap_if_applicable(kernel, recv_tid, cap, endpoint);
    // Stage 4C/4D/4J: attempt immediate split recv; fallback to full ipc_recv path.
    let (received, split_scheduler_plan) =
        if let Some((msg, plan)) = try_endpoint_split_recv(kernel, endpoint)? {
            (Some(msg), plan)
        } else {
            (
                kernel.ipc_recv(cap).map_err(SyscallError::from)?,
                IpcSchedulerPlan::None,
            )
        };
    // Apply deferred scheduler plan: wake sender whose message was refilled into the
    // endpoint queue under ipc_state_lock (Stage 4D). Lock is released; safe to wake.
    if let IpcSchedulerPlan::WakeSender(wake_tid) = split_scheduler_plan {
        let _ = kernel.apply_split_sender_wake_plan(wake_tid);
    }
    // Stage 35: use canonical meta_target to detect recv-v2 instead of raw frame
    // arg checks — semantically identical to the previous inline check.
    let recv_v2_request = matches!(request.meta_target, RecvMetaTarget::V2 { .. });
    if received.is_none() {
        if recv_v2_request {
            let (meta_user_ptr, meta_user_len) = match request.meta_target {
                RecvMetaTarget::V2 { ptr, len } => (ptr, len),
                _ => unreachable!("recv_v2_request is true only when meta_target is V2"),
            };
            let state = BlockedRecvState {
                recv_cap: cap,
                payload_user_ptr: frame.arg(SYSCALL_ARG_PTR),
                payload_user_len: frame.arg(SYSCALL_ARG_LEN),
                meta_user_ptr,
                meta_user_len,
                recv_abi: RecvAbiVariant::RecvV2,
            };
            kernel.with_tcb_mut(recv_tid, |tcb| {
                tcb.blocked_recv_state = Some(state);
            });
            crate::yarm_log!(
                "IPC_RECV_BLOCKED_STATE_SAVE tid={} cap={} payload_ptr=0x{:x} payload_len={} meta_ptr=0x{:x} meta_len={}",
                recv_tid,
                cap.0,
                state.payload_user_ptr,
                state.payload_user_len,
                state.meta_user_ptr,
                state.meta_user_len
            );
            // Stage 198E3C1B-H: publish the AUTHORITATIVE blocked-recv acknowledgement now that the
            // blocked-recv record is FULLY committed — the endpoint waiter was linked + the task
            // marked Blocked inside `ipc_recv` (Phases B/C), and `BlockedRecvState` (payload/meta)
            // was just stored above. This is the earliest point the complete committed identity
            // exists; it is oracle-only (feature+knob gated), reads authoritative committed state,
            // and does not wake / mint / copy / lock / retire. A strict no-op off the oracle.
            #[cfg(feature = "shared-region-direct-oracle")]
            crate::kernel::boot::maybe_publish_shared_region_blocked_recv_ack(
                kernel, recv_tid, endpoint, &state,
            );
            // Stage 199A2B2F: publish the NR6 committed blocked-server acknowledgement
            // from the same fully-committed recv-v2 point (proof-gated; no wake / mint /
            // copy / scheduler mutation / retirement marker).
            crate::kernel::boot::maybe_publish_ipccall_direct_blocked_server_ack(
                kernel, recv_tid, endpoint, &state,
            );
            // Stage 199A2B3: publish the NR7 committed blocked-CALLER acknowledgement
            // from the same fully-committed recv-v2 point (caller blocking on its reply
            // endpoint). Proof-gated; no wake / mint / copy / scheduler mutation.
            crate::kernel::boot::maybe_publish_ipcreply_direct_blocked_caller_ack(
                kernel, recv_tid, endpoint, &state,
            );
        }
        return Err(SyscallError::WouldBlock);
    }
    clear_blocked_recv_state(kernel, recv_tid, "immediate_success");
    crate::yarm_log!(
        "IPC_RECV_GOT_MSG tid={} cap={} transfer_cap={}",
        recv_tid,
        cap.0,
        received
            .as_ref()
            .and_then(|m| m.transferred_cap())
            .map(|c| c.0)
            .unwrap_or(u64::MAX)
    );
    handle_ipc_recv_result(
        kernel,
        frame,
        endpoint,
        frame.arg(SYSCALL_ARG_PTR),
        frame.arg(SYSCALL_ARG_LEN),
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
        received,
    )
}

pub(super) fn handle_ipc_recv_timeout(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    use crate::kernel::recv_core::{RecvBlockingPolicy, RecvRequest};

    let cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let recv_tid = kernel.current_tid().unwrap_or(0);
    validate_endpoint_right(kernel, cap, CapRights::RECEIVE)?;
    let endpoint_cap = kernel
        .current_task_cnode()
        .and_then(|cnode| kernel.capability_for_cnode_local(cnode, cap))
        .and_then(|capability| {
            kernel
                .capability_object_live(capability.object)
                .map(|_| capability)
        });
    let Some(endpoint_cap) = endpoint_cap else {
        crate::yarm_log!(
            "IPC_RECV_INVALID_CAP_SOURCE reason=timeout_post_validate_endpoint_lookup tid={} cap={} endpoint={}",
            recv_tid,
            cap.0,
            u64::MAX
        );
        return Err(SyscallError::InvalidCapability);
    };
    let endpoint = endpoint_cap.object;
    crate::yarm_log!(
        "IPC_RECV_AFTER_CAP_OK tid={} cap={} endpoint={:?}",
        recv_tid,
        cap.0,
        endpoint
    );
    log_supervisor_fault_recv_cap_if_applicable(kernel, recv_tid, cap, endpoint);
    let timeout_ticks = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0) as u64;
    let user_ptr = frame.arg(SYSCALL_ARG_PTR);
    let user_len = frame.arg(SYSCALL_ARG_LEN);
    let waiter_tid = current_tid(kernel)?;
    clear_blocked_recv_state(kernel, waiter_tid, "error");
    // Consume the per-CPU pre-read deadline from the split-read optimization path.
    // When handle_trap_entry_shared (arch/trap_entry.rs) detects this syscall
    // before acquiring the global lock, it pre-reads the scheduler tick under the
    // lighter scheduler lock and stores an absolute deadline here.  Using that
    // pre-computed deadline avoids a redundant tick read inside the global lock.
    let preread_deadline: Option<u64> = {
        let cpu_idx = kernel.current_cpu().0 as usize;
        if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
            let v = crate::kernel::scheduler::SPLIT_RECV_TIMEOUT_DEADLINE[cpu_idx]
                .swap(0, core::sync::atomic::Ordering::AcqRel);
            if v != 0 { Some(v) } else { None }
        } else {
            None
        }
    };
    // Stage 35: build canonical request for decode/planning.  The adapter
    // captures timeout_ticks==0 as NonblockingProbe/NoWait and timeout_ticks>0
    // as TimedRecv/Deadline.  We use request.blocking below to replace the
    // inline timeout_ticks==0 check; deadline fallback logic is unchanged.
    let is_kernel_task = matches!(current_task_has_user_asid(kernel), Ok(false));
    let request = RecvRequest::from_ipc_recv_timeout(
        recv_tid as u64,
        cap,
        user_ptr,
        user_len,
        timeout_ticks,
        preread_deadline,
        is_kernel_task,
    );
    crate::yarm_log!(
        "YARM_RECV_CORE_ADAPTER kind=legacy_timeout is_kernel_task={} blocking={:?}",
        is_kernel_task,
        request.blocking
    );
    // Stage 4G/4I/4J: try the split recv path regardless of timeout_ticks.
    // If a plain message is already queued, delivery is immediate — the deadline is
    // irrelevant.  Ineligible cases (non-plain message, complex sender state, empty
    // queue) fall through to the appropriate timed/blocking path.
    let mut try_recv_scheduler_plan = IpcSchedulerPlan::None;
    let mut split_recv_succeeded = false;
    let received = if let Some((msg, plan)) = try_endpoint_split_recv(kernel, endpoint)? {
        split_recv_succeeded = true;
        try_recv_scheduler_plan = plan;
        Some(msg)
    } else {
        // Stage 35: use request.blocking to classify the timeout case instead of
        // the raw timeout_ticks==0 check.  Deadline logic is preserved as-is:
        // preread_deadline takes priority over ipc_recv_with_deadline.
        match request.blocking {
            RecvBlockingPolicy::NoWait => {
                // Stage 168 (D2-GENUINE-RECV): non-blocking probe preserves the
                // existing immediate try-recv semantics (no block, no dispatch).
                let r = kernel.try_ipc_recv(cap).map_err(SyscallError::from)?;
                if crate::kernel::boot::d2_recv_genuine_enabled() {
                    crate::yarm_log!("D2_RECV_GENUINE_NOWAIT_OK tid={}", recv_tid);
                }
                r
            }
            RecvBlockingPolicy::Deadline(_) => {
                if let Some(deadline) = preread_deadline {
                    kernel
                        .ipc_recv_until_deadline(cap, deadline)
                        .map_err(SyscallError::from)?
                } else {
                    kernel
                        .ipc_recv_with_deadline(cap, timeout_ticks)
                        .map_err(SyscallError::from)?
                }
            }
            RecvBlockingPolicy::WaitForever => {
                // ipc_recv_timeout never produces WaitForever; treat as timed.
                kernel
                    .ipc_recv_with_deadline(cap, timeout_ticks)
                    .map_err(SyscallError::from)?
            }
        }
    };
    // Apply deferred scheduler plan from Stage 4D/4G/4I split recv refill if any.
    if let IpcSchedulerPlan::WakeSender(wake_tid) = try_recv_scheduler_plan {
        let _ = kernel.apply_split_sender_wake_plan(wake_tid);
    }
    // Skip the timeout-fired check when the split path already delivered a message.
    // A stale ipc_timeout_fired flag from a prior syscall must not corrupt the result
    // of an immediate split recv that succeeded before any blocking occurred.
    let timed_out =
        if matches!(request.blocking, RecvBlockingPolicy::NoWait) || split_recv_succeeded {
            false
        } else {
            let fired = kernel
                .consume_ipc_timeout_fired_for_tid(waiter_tid)
                .map_err(SyscallError::from)?;
            fired || received.is_none()
        };
    if timed_out {
        clear_blocked_recv_state(kernel, waiter_tid, "timeout");
    } else if received.is_some() {
        clear_blocked_recv_state(kernel, waiter_tid, "immediate_success");
    }
    handle_ipc_recv_result_with_empty_error(
        kernel,
        frame,
        endpoint,
        user_ptr,
        user_len,
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
        frame.arg(SYSCALL_ARG_TRANSFER_CAP),
        received,
        if timed_out {
            SyscallError::TimedOut
        } else {
            SyscallError::WouldBlock
        },
    )
}

pub(super) fn handle_ipc_call(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    validate_endpoint_right(kernel, cap, CapRights::SEND)?;
    let endpoint = kernel
        .capability_service()
        .resolve_current_task_capability(cap)
        .ok_or(SyscallError::InvalidCapability)?
        .object;

    let reply_recv_cap = CapId(frame.arg(SYSCALL_ARG_TRANSFER_CAP) as u64);
    validate_endpoint_right(kernel, reply_recv_cap, CapRights::RECEIVE)?;
    let sender_tid = current_tid(kernel)?;
    crate::yarm_log!(
        "IPC_CALL_BEGIN tid={} send_cap={} reply_cap={}",
        sender_tid,
        cap.0,
        reply_recv_cap.0
    );
    let responder_tid = kernel.endpoint_waiter_tid(endpoint);
    let endpoint_idx = kernel
        .resolve_endpoint_index(endpoint)
        .map_err(SyscallError::from)?;
    if let Some(waiter_tid) = responder_tid {
        crate::yarm_log!(
            "IPC_CALL_WAKE_RECEIVER endpoint={} tid={}",
            endpoint_idx,
            waiter_tid.0
        );
    }

    // Stage 199A2A (request copy-before-reserve): copy the request payload into an
    // OWNED kernel buffer BEFORE reserving any reply-cap record or minting the caller
    // Reply cap. Previously the reply cap was created first and a user-copy fault (or
    // an oversized `len`) returned WITHOUT freeing the reserved global ReplyCapRecord
    // and the minted caller cnode slot, leaking one of each per faulting IpcCall.
    // Copying first means a copy fault (or bad length) leaves NO record / cap /
    // delivery / wake to unwind — the fault path returns with zero reply-cap state
    // mutated. No userspace pointer is retained past this copy; the owned
    // `payload_bytes` is the sole source for the delivered message.
    let user_ptr_or_offset = frame.arg(SYSCALL_ARG_PTR);
    let len = frame.arg(SYSCALL_ARG_LEN);
    if len > Message::MAX_PAYLOAD {
        return Err(SyscallError::InvalidArgs);
    }
    let payload_bytes: [u8; Message::MAX_PAYLOAD] = if current_task_has_user_asid(kernel)? {
        let payload = match kernel.copy_from_current_user(user_ptr_or_offset, len) {
            Ok(payload) => payload,
            Err(KernelError::UserMemoryFault) => {
                record_user_fault(kernel, frame, user_ptr_or_offset, FaultAccess::Read);
                return Ok(());
            }
            Err(other) => return Err(SyscallError::from(other)),
        };
        let mut out = [0u8; Message::MAX_PAYLOAD];
        out[..len].copy_from_slice(&payload[..len]);
        out
    } else {
        inline_payload_from_frame(frame, len)?
    };

    // Now that the request payload is safely owned, reserve the global reply-cap
    // record and mint the caller Reply cap. A failure here unwinds no payload state.
    let reply_cap = kernel
        .create_reply_cap_for_caller(
            crate::kernel::ipc::ThreadId(sender_tid),
            reply_recv_cap,
            responder_tid,
        )
        .map_err(|err| {
            crate::yarm_log!(
                "IPC_CALL_FAIL stage=reply_cap_alloc err={:?} caller_tid={} endpoint={}",
                err,
                sender_tid,
                endpoint_idx
            );
            SyscallError::from(err)
        })?;
    let reply_obj = kernel
        .resolve_capability_for_task(sender_tid, reply_cap)
        .map(|cap| cap.object)
        .ok();
    crate::yarm_log!(
        "IPC_CALL_REPLY_CAP_CREATE caller_tid={} waiter_tid={} reply_obj={:?}",
        sender_tid,
        responder_tid.map(|tid| tid.0).unwrap_or(u64::MAX),
        reply_obj
    );

    // Stash the transfer envelope (binds the reply cap to the endpoint) and build the
    // kernel message from the OWNED payload buffer — never from a userspace pointer.
    let (transfer_handle, stash_bound_receiver_tid) =
        stash_transfer_handle(kernel, Some(reply_cap), endpoint, None)?;
    let msg = Message::with_header(
        sender_tid,
        OPCODE_INLINE,
        Message::FLAG_REPLY_CAP,
        transfer_handle,
        &payload_bytes[..len],
    )
    .map_err(|_| SyscallError::InvalidArgs)?;

    // Stage 4L: IpcCall to a recv-v2 blocked receiver — complete delivery outside
    // ipc_state_lock using the same Phase 1-5 protocol as Stage 4K (IpcSend).
    // ipc_try_send_queued_plain_endpoint_only returns ReceiverWaiterFound for
    // FLAG_REPLY_CAP messages when a receiver waiter is present (the flag check
    // only applies to the no-waiter enqueue path). complete_blocked_recv_for_waiter
    // handles FLAG_REPLY_CAP via materialize_received_message_cap.
    //
    // VALIDATION: LIVE_OFF_TRAP
    // VALIDATION: SPLIT_FAST_PATH_ONLY
    // Stage 101: live-wired off the trap entry; non-recv-v2 receivers and
    // sender/cap-transfer envelope failures fall back to the global-lock
    // `kernel.ipc_send(...)` path. See doc/KERNEL_UNLOCKING.md
    //
    // The transfer envelope was stashed by stash_transfer_handle with
    // receiver_tid = Some(waiter_tid). Error-path cleanup must pass that same
    // receiver_tid to take_transfer_envelope — passing sender_tid would cause
    // the bound-receiver check to fail and the envelope to leak.
    let call_split_wake = match kernel.ipc_try_send_queued_plain_endpoint_only(endpoint_idx, msg) {
        IpcEndpointSendResult::ReceiverWaiterFound(receiver) => {
            let receiver_tid = receiver.tid;
            let is_recv_v2 = kernel.is_task_recv_v2_blocked(receiver_tid.0);
            if is_recv_v2 {
                // Stage 188E: wire the reply-cap producer (188D rank-inversion
                // seam) LIVE for the ipc_call blocked recv-v2 path — the real
                // reply-cap→blocked-waiter path 188D built the seam for. Phase A
                // (here, under the broad borrow) takes the reply-cap envelope once
                // and stashes a by-value BlockedWaiterReplyCapDelivery; the
                // dispatch-return executor mints the receiver-local reply cap
                // (rank 4), records the waiter-cap (rank 3), copies payload+meta
                // (186E), and clears + wakes the receiver — all AFTER the broad
                // borrow drops. Here we only account the split and set the CALLER's
                // return frame. Non-reply-cap / no-drainer → Ok(false) (legacy).
                match crate::kernel::syscall::produce_blocked_waiter_reply_cap_delivery(
                    kernel,
                    receiver_tid.0,
                    endpoint_idx,
                    &msg,
                ) {
                    Ok(true) => {
                        kernel.note_ipc_call_split_delivery();
                        crate::yarm_log!(
                            "IPC_CALL_SPLIT_DELIVERY tid={} receiver={} endpoint={}",
                            sender_tid,
                            receiver_tid.0,
                            endpoint_idx
                        );
                        // Caller (sender) returns Ok now; the receiver slot-clear +
                        // wake are done by the dispatch-return executor (Phase C).
                        frame.set_ok(0, 0, 0);
                        encode_transfer_cap_ret(frame, None)?;
                        return Ok(());
                    }
                    Ok(false) => { /* not reply cap / no drainer — legacy path below */ }
                    Err(e) => {
                        // Same envelope disposition as the legacy Err arm: the
                        // envelope was bound to receiver_tid at stash time (a
                        // producer Phase-A fault before the take leaves it, so this
                        // cleanup consumes it; after the take it is already gone and
                        // this is a no-op).
                        if let Some(handle) = msg.transferred_cap().map(|c| c.0) {
                            let _ = kernel.take_transfer_envelope(handle, endpoint, receiver_tid);
                        }
                        return Err(e);
                    }
                }
                // Phase 3: complete delivery outside ipc_state_lock (legacy path
                // when no trap-entry drainer is active).
                match complete_blocked_recv_for_waiter(kernel, receiver_tid.0, &msg) {
                    Ok(()) => {
                        // Phase 4: clear waiter slot under ipc_state_lock.
                        kernel.ipc_clear_plain_receiver_waiter_only(endpoint_idx, receiver_tid);
                        kernel.note_ipc_call_split_delivery();
                        crate::yarm_log!(
                            "IPC_CALL_SPLIT_DELIVERY tid={} receiver={} endpoint={}",
                            sender_tid,
                            receiver_tid.0,
                            endpoint_idx
                        );
                        Some(receiver_tid)
                    }
                    Err(e) => {
                        // Use receiver_tid (not sender_tid) — the envelope was
                        // stashed with receiver_tid bound to the waiter.
                        if let Some(handle) = msg.transferred_cap().map(|c| c.0) {
                            let _ = kernel.take_transfer_envelope(handle, endpoint, receiver_tid);
                        }
                        return Err(e);
                    }
                }
            } else {
                None
            }
        }
        _ => None,
    };

    if let Some(recv_tid) = call_split_wake {
        // Phase 5: wake receiver outside ipc_state_lock.
        crate::yarm_log!(
            "IPC_CALL_SENT_OR_QUEUED tid={} endpoint={}",
            sender_tid,
            endpoint_idx
        );
        // IPC_CALL is request-send only in the current userspace contract. The
        // caller receives replies via an explicit recv on reply_recv_cap.
        frame.set_ok(0, 0, 0);
        encode_transfer_cap_ret(frame, None)?;
        let _ = kernel.apply_split_receiver_wake_plan(recv_tid);
        return Ok(());
    }

    if let Err(err) = kernel.ipc_send(cap, msg) {
        if let Some(handle) = msg.transferred_cap().map(|c| c.0) {
            // Use the receiver TID bound at stash time — sender_tid would fail
            // the bound-receiver check when a waiter was present at stash time.
            let cleanup_tid =
                stash_bound_receiver_tid.unwrap_or(crate::kernel::ipc::ThreadId(sender_tid));
            let _ = kernel.take_transfer_envelope(handle, endpoint, cleanup_tid);
        }
        return Err(SyscallError::from(err));
    }
    crate::yarm_log!(
        "IPC_CALL_SENT_OR_QUEUED tid={} endpoint={}",
        sender_tid,
        endpoint_idx
    );
    // IPC_CALL is request-send only in the current userspace contract. The caller
    // receives replies via an explicit recv on reply_recv_cap (ipc_recv_v2 /
    // ipc_recv_with_deadline), so the call syscall must not consume/decode reply
    // payload bytes here.
    frame.set_ok(0, 0, 0);
    encode_transfer_cap_ret(frame, None)?;
    Ok(())
}

// VALIDATION: GLOBAL_LOCK_SLOW_PATH
// Stage 101: NR 7 IpcReply is not yet split-wired off the trap-entry seam.
// kernel.ipc_reply(...) runs under the global &mut KernelState. A future
// Stage 102+ may add a Stage-4M fast path analogous to Stage 4L.
// See doc/KERNEL_UNLOCKING.md
pub(super) fn handle_ipc_reply(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let reply_cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let user_ptr = frame.arg(SYSCALL_ARG_PTR);
    let len = frame.arg(SYSCALL_ARG_LEN);
    let sender_tid = current_tid(kernel)?;

    // ── Transfer-cap argument (arg5) ──────────────────────────────────────────
    // The user-space `ipc_reply` wrapper passes the transferred cap's CapId as
    // the last syscall argument (SYSCALL_ARG_TRANSFER_CAP).  We validate it
    // eagerly so that any error is surfaced before we copy the payload from user
    // memory (which may fault).
    let transfer_cap = transfer_cap_arg(kernel, frame)?;
    if let Some(c) = transfer_cap {
        validate_transfer_cap(kernel, c)?;
    }

    crate::yarm_log!(
        "IPC_REPLY_ENTER tid={} reply_cap={} len={} transfer_cap={}",
        sender_tid,
        reply_cap.0,
        len,
        transfer_cap.map(|c| c.0).unwrap_or(SYSCALL_NO_TRANSFER_CAP),
    );
    let cnode = kernel.current_task_cnode();
    let slot_result = cnode.and_then(|cn| kernel.capability_for_cnode_local(cn, reply_cap));
    let live_result = slot_result.and_then(|c| kernel.capability_object_live(c.object).map(|_| c));
    crate::yarm_log!(
        "IPC_REPLY_CAP_PROBE tid={} cap={} cnode={} slot_found={} object_live={} object={:?} rights={:?}",
        sender_tid,
        reply_cap.0,
        cnode.map(|c| c.0).unwrap_or(u64::MAX),
        slot_result.is_some(),
        live_result.is_some(),
        live_result.map(|c| c.object),
        live_result.map(|c| c.rights()),
    );
    if len > Message::MAX_PAYLOAD {
        return Err(SyscallError::InvalidArgs);
    }

    // Stage 200C2A: oracle-gated NR7 reply-win deadline-disarm hook. Runs BEFORE the
    // user payload copy (holds no deadline-queue lock over any copy) and is a strict
    // no-op off the oracle / off the confined reply endpoint / when timeout already
    // won. This is the ONLY NR7 integration; the frozen reply flow below is unchanged.
    #[cfg(feature = "x86-ipc-reply-timeout-oracle")]
    kernel.maybe_win_reply_terminal_on_reply(reply_cap);

    // ── Build raw payload bytes ────────────────────────────────────────────────
    let payload_bytes: [u8; Message::MAX_PAYLOAD] = if current_task_has_user_asid(kernel)? {
        let payload = match kernel.copy_from_current_user(user_ptr, len) {
            Ok(payload) => payload,
            Err(KernelError::UserMemoryFault) => {
                record_user_fault(kernel, frame, user_ptr, FaultAccess::Read);
                return Ok(());
            }
            Err(other) => return Err(SyscallError::from(other)),
        };
        let mut out = [0u8; Message::MAX_PAYLOAD];
        out[..len].copy_from_slice(&payload[..len]);
        out
    } else {
        inline_payload_from_frame(frame, len)?
    };

    // ── Stash transfer envelope if a cap is being forwarded ───────────────────
    //
    // For reply-with-cap we need to bind the transfer envelope to the endpoint
    // that the original caller is waiting on.  We peek the reply endpoint from
    // the ReplyCapRecord *before* calling `ipc_reply` (which would consume the
    // record).
    //
    // We use `FLAG_CAP_TRANSFER_PLAIN` (bit 2) rather than the standard
    // `FLAG_CAP_TRANSFER` (bit 0).  `FLAG_CAP_TRANSFER` triggers
    // `should_strip_inline_opcode_prefix` on the receiver side, which assumes
    // the sender prepended a 2-byte opcode in the payload (the ipc_send/
    // ipc_call protocol).  Reply messages carry the payload bytes verbatim
    // without any such prefix; using FLAG_CAP_TRANSFER_PLAIN avoids the
    // destructive 2-byte strip and preserves the full payload for the receiver.
    let mut stash_bound_reply_tid: Option<crate::kernel::ipc::ThreadId> = None;
    // Captured here for the failure cleanup path: ipc_reply consumes and revokes the
    // reply cap record (including fast_revoke_reply_cap_in_cnode), so re-probing
    // reply_cap_peek_endpoint after a failed ipc_reply would fail and leak the envelope.
    let mut reply_endpoint_for_cleanup: Option<CapObject> = None;
    let transfer_handle = if transfer_cap.is_some() {
        let reply_endpoint = kernel
            .reply_cap_peek_endpoint(reply_cap)
            .map_err(SyscallError::from)?;
        reply_endpoint_for_cleanup = Some(reply_endpoint);
        let (handle, bound_tid) =
            stash_transfer_handle(kernel, transfer_cap, reply_endpoint, None)?;
        stash_bound_reply_tid = bound_tid;
        crate::yarm_log!(
            "IPC_REPLY_WITH_CAP_STASH tid={} transfer_cap={} handle={} endpoint={:?}",
            sender_tid,
            transfer_cap.map(|c| c.0).unwrap_or(0),
            handle.unwrap_or(SYSCALL_NO_TRANSFER_CAP),
            reply_endpoint,
        );
        handle
    } else {
        None
    };

    // ── Build the kernel IPC message ──────────────────────────────────────────
    let msg = if let Some(handle) = transfer_handle {
        Message::with_header(
            sender_tid,
            OPCODE_INLINE,
            Message::FLAG_CAP_TRANSFER_PLAIN,
            Some(handle),
            &payload_bytes[..len],
        )
        .map_err(|_| SyscallError::InvalidArgs)?
    } else {
        Message::new(sender_tid, &payload_bytes[..len]).map_err(|_| SyscallError::InvalidArgs)?
    };

    crate::yarm_log!(
        "IPC_REPLY_DELIVER len={} opcode={} flags={} has_cap={}",
        msg.len,
        msg.opcode,
        msg.flags,
        transfer_handle.is_some(),
    );
    if let Err(err) = kernel.ipc_reply(reply_cap, msg) {
        // If ipc_reply failed and we stashed a transfer envelope, clean it up.
        // Use the endpoint captured before ipc_reply: ipc_reply revokes the reply
        // cap cnode slot on the fast path, so re-probing reply_cap_peek_endpoint
        // here would fail and silently leave the envelope allocated.
        if let Some(handle) = transfer_handle {
            if let Some(reply_endpoint) = reply_endpoint_for_cleanup {
                let cleanup_tid =
                    stash_bound_reply_tid.unwrap_or(crate::kernel::ipc::ThreadId(sender_tid));
                let _ = kernel.take_transfer_envelope(handle, reply_endpoint, cleanup_tid);
            }
        }
        if err == KernelError::WrongObject {
            let cnode = kernel.current_task_cnode();
            let slot_cap = cnode.and_then(|cn| kernel.capability_for_cnode_local(cn, reply_cap));
            crate::yarm_log!(
                "IPC_REPLY_WRONG_OBJECT tid={} reply_cap={} object={:?} rights={:?}",
                sender_tid,
                reply_cap.0,
                slot_cap.map(|c| c.object),
                slot_cap.map(|c| c.rights())
            );
        }
        let mapped = SyscallError::from(err);
        crate::yarm_log!(
            "IPC_REPLY_FAIL tid={} reply_cap={} err={:?}",
            sender_tid,
            reply_cap.0,
            mapped
        );
        return Err(mapped);
    }
    frame.set_ok(0, 0, 0);
    encode_transfer_cap_ret(frame, None)?;
    Ok(())
}

fn handle_ipc_recv_result(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
    endpoint: CapObject,
    user_ptr: usize,
    user_len: usize,
    meta_ptr: usize,
    meta_len: usize,
    received: Option<Message>,
) -> Result<(), SyscallError> {
    handle_ipc_recv_result_with_empty_error(
        kernel,
        frame,
        endpoint,
        user_ptr,
        user_len,
        meta_ptr,
        meta_len,
        received,
        SyscallError::WouldBlock,
    )
}

fn handle_ipc_recv_result_with_empty_error(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
    endpoint: CapObject,
    user_ptr: usize,
    user_len: usize,
    meta_ptr: usize,
    meta_len: usize,
    received: Option<Message>,
    empty_error: SyscallError,
) -> Result<(), SyscallError> {
    match received {
        Some(msg) => {
            let recv_meta_flags = if (msg.flags & Message::FLAG_REPLY_CAP) != 0 {
                SYSCALL_RECV_META_REPLY_CAP
            } else if (msg.flags & (Message::FLAG_CAP_TRANSFER | Message::FLAG_CAP_TRANSFER_PLAIN))
                != 0
            {
                SYSCALL_RECV_META_TRANSFERRED_CAP
            } else {
                0
            };
            let sender = sender_tid_to_ret(msg.sender_tid.0)?;
            let receiver_tid = current_tid(kernel)?;
            let raw_transfer_cap = msg.transferred_cap().map(|c| c.0);
            let recv_local_transfer = match materialize_received_message_cap(
                kernel,
                endpoint,
                receiver_tid,
                msg.sender_tid.0,
                &msg,
            ) {
                Ok(local_cap) => {
                    if let Some(raw) = raw_transfer_cap {
                        crate::yarm_log!(
                            "IPC_RECV_IMMEDIATE_TRANSFER_CAP_MINT tid={} local_cap={} raw={}",
                            receiver_tid,
                            local_cap.unwrap_or(SYSCALL_NO_TRANSFER_CAP),
                            raw
                        );
                    }
                    local_cap
                }
                Err(err) => {
                    crate::yarm_log!(
                        "IPC_RECV_IMMEDIATE_TRANSFER_CAP_MINT_FAILED tid={} raw={} err={:?}",
                        receiver_tid,
                        raw_transfer_cap.unwrap_or(SYSCALL_NO_TRANSFER_CAP),
                        err
                    );
                    return Err(err);
                }
            };
            encode_transfer_cap_ret(frame, recv_local_transfer)?;
            crate::yarm_log!(
                "IPC_RECV_IMMEDIATE_META_CAP tid={} cap={} flags={}",
                receiver_tid,
                recv_local_transfer.unwrap_or(SYSCALL_NO_TRANSFER_CAP),
                recv_meta_flags
            );
            let raw_payload = msg.as_slice();
            let (app_opcode, app_payload, _stripped_prefix) =
                if should_strip_inline_opcode_prefix(&msg) && raw_payload.len() >= 2 {
                    (
                        u16::from_le_bytes([raw_payload[0], raw_payload[1]]),
                        &raw_payload[2..],
                        1usize,
                    )
                } else {
                    (msg.opcode, raw_payload, 0usize)
                };
            let recv_v2_meta_written = meta_ptr != 0 && meta_len >= IPC_RECV_META_V2_ENCODED_LEN;
            if recv_v2_meta_written {
                // recv-v2: write metadata struct to the caller's meta buffer.
                // ret0 will be 0 (success) since all metadata goes into the meta struct.
                // Stage 155: byte-identical to the prior inline encoding; the
                // status word ([0..8]) carries `sender` and [10..12] carries
                // `msg.flags` for this immediate full-recv path.
                let meta = super::ipc_recv_core::encode_recv_v2_meta(
                    sender as u64,
                    app_opcode,
                    msg.flags,
                    app_payload.len() as u32,
                    frame.ret2() as u64,
                    recv_meta_flags as u64,
                    msg.sender_tid.0,
                );
                crate::yarm_log!(
                    "IPC_RECV_OUT_META_REPLY status={} opcode={} len={} flags={} sender_tid={}",
                    sender,
                    app_opcode,
                    app_payload.len(),
                    msg.flags,
                    msg.sender_tid.0
                );
                if let Err(copy_err) = kernel.copy_to_current_user(meta_ptr, &meta) {
                    // Stage 20: the cap was materialized into this (receiver/current)
                    // task's cnode before this meta copy faulted.  Roll it back so the
                    // dropped delivery does not leak a cnode slot / cap_refcount.
                    if let Some(materialized) = recv_local_transfer {
                        let is_reply = (msg.flags & Message::FLAG_REPLY_CAP) != 0;
                        kernel.rollback_materialized_recv_cap(
                            receiver_tid,
                            CapId(materialized),
                            is_reply,
                        );
                        let _ = encode_transfer_cap_ret(frame, None);
                        // Stage 156 IPC oracle: rollback on immediate meta-copy fault.
                        crate::yarm_log!(
                            "IPC_RECV_V2_ROLLBACK_OK site=immediate_meta reply={}",
                            is_reply
                        );
                    }
                    return Err(SyscallError::from(copy_err));
                }
                // Stage 156 IPC oracle: immediate full-recv recv-v2 meta delivered.
                crate::yarm_log!("IPC_RECV_V2_META_IMMEDIATE_OK len=40");
            }

            if current_task_has_user_asid(kernel)? {
                if msg.opcode == OPCODE_SHARED_MEM {
                    let desc = SharedMemoryRegion::decode(msg.as_slice())
                        .ok_or(SyscallError::InvalidArgs)?;
                    let region_len =
                        usize::try_from(desc.len).map_err(|_| SyscallError::InvalidArgs)?;
                    if user_ptr == 0 || user_len < region_len {
                        if frame.ret2() as u64 != SYSCALL_NO_TRANSFER_CAP {
                            revoke_current_transfer_cap_best_effort(
                                kernel,
                                CapId(frame.ret2() as u64),
                            );
                            encode_transfer_cap_ret(frame, None)?;
                        }
                        return Err(SyscallError::InvalidArgs);
                    }
                    let transfer_cap_raw =
                        u64::try_from(frame.ret2()).map_err(|_| SyscallError::InvalidArgs)?;
                    if transfer_cap_raw == SYSCALL_NO_TRANSFER_CAP {
                        return Err(SyscallError::InvalidArgs);
                    }
                    let transfer_cap = CapId(transfer_cap_raw);
                    let recv_map_flags = match recv_shared_mem_map_intent_flags(frame) {
                        Ok(flags) => flags,
                        Err(err) => {
                            revoke_current_transfer_cap_best_effort(kernel, transfer_cap);
                            encode_transfer_cap_ret(frame, None)?;
                            return Err(err);
                        }
                    };
                    let transfer_capability = kernel
                        .capability_service()
                        .resolve_current_task_capability(transfer_cap)
                        .ok_or(SyscallError::InvalidCapability)?;
                    if recv_map_flags.write && !transfer_capability.has_right(CapRights::WRITE) {
                        revoke_current_transfer_cap_best_effort(kernel, transfer_cap);
                        encode_transfer_cap_ret(frame, None)?;
                        return Err(SyscallError::MissingRight);
                    }
                    let transfer_cap = attenuate_transfer_cap_for_recv_intent(
                        kernel,
                        transfer_cap,
                        recv_map_flags.write,
                    )?;
                    if transfer_cap.0 != transfer_cap_raw {
                        encode_transfer_cap_ret(frame, Some(transfer_cap.0))?;
                    }
                    let (mapped_va, mapped_len) = match map_shared_region_into_receiver(
                        kernel,
                        transfer_cap,
                        user_ptr,
                        region_len,
                        recv_map_flags,
                    ) {
                        Ok(mapped) => mapped,
                        Err(err) => {
                            revoke_current_transfer_cap_best_effort(kernel, transfer_cap);
                            encode_transfer_cap_ret(frame, None)?;
                            return Err(err);
                        }
                    };
                    // Stage 7: plan-first ASID for the register_active_transfer_mapping
                    // rollback below. current_task_has_user_asid (checked above) guarantees
                    // task_asid returns Some. Captured by the map_err closure as Copy.
                    let receiver_asid = kernel
                        .task_asid(receiver_tid)
                        .ok_or(SyscallError::from(KernelError::UserMemoryFault))?;
                    kernel
                        .register_active_transfer_mapping(
                            crate::kernel::ipc::ThreadId(receiver_tid),
                            transfer_cap,
                            VirtAddr(mapped_va as u64),
                            mapped_len,
                        )
                        .map_err(|e| {
                            // Stage 7: two-phase rollback — reclaim only after shootdown.
                            let mut rollback = mapped_va;
                            let end = mapped_va.saturating_add(mapped_len);
                            while rollback < end {
                                if let Ok(Some(plan)) = kernel
                                    .unmap_page_phase1(receiver_asid, VirtAddr(rollback as u64))
                                {
                                    let _ = kernel.execute_tlb_shootdown_wait_plan(plan);
                                }
                                rollback += PAGE_SIZE;
                            }
                            revoke_current_transfer_cap_best_effort(kernel, transfer_cap);
                            let _ = encode_transfer_cap_ret(frame, None);
                            SyscallError::from(e)
                        })?;
                    kernel.note_shared_mem_mapped(mapped_len);
                    frame.set_ok(0, mapped_len, frame.ret2());
                    frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD0, mapped_va);
                    frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD1, region_len);
                    return Ok(());
                }

                if user_len < app_payload.len() {
                    // Stage 20: roll back the already-materialized cap before
                    // dropping the message for an undersized user buffer.
                    if let Some(materialized) = recv_local_transfer {
                        let is_reply = (msg.flags & Message::FLAG_REPLY_CAP) != 0;
                        kernel.rollback_materialized_recv_cap(
                            receiver_tid,
                            CapId(materialized),
                            is_reply,
                        );
                        let _ = encode_transfer_cap_ret(frame, None);
                    }
                    return Err(SyscallError::InvalidArgs);
                }
                match kernel.copy_to_current_user(user_ptr, app_payload) {
                    Ok(()) => {
                        crate::yarm_log!(
                            "IPC_RECV_COPY_TO_USER tid={} dst=0x{:x} len={} result=ok",
                            receiver_tid,
                            user_ptr,
                            app_payload.len()
                        );
                        // In recv-v2 mode, all metadata is in the out-meta struct;
                        // ret0 is 0 (success). In legacy mode, ret0 is sender TID.
                        let ret0 = if recv_v2_meta_written { 0 } else { sender };
                        frame.set_ok(ret0, app_payload.len(), frame.ret2());
                    }
                    Err(KernelError::UserMemoryFault) => {
                        crate::yarm_log!(
                            "IPC_RECV_COPY_TO_USER tid={} dst=0x{:x} len={} result=err",
                            receiver_tid,
                            user_ptr,
                            app_payload.len()
                        );
                        record_user_fault(kernel, frame, user_ptr, FaultAccess::Write);
                        return Ok(());
                    }
                    Err(other) => return Err(SyscallError::from(other)),
                };
            } else {
                // Kernel task (no user ASID): return full raw payload in inline registers.
                // Do not apply opcode-prefix stripping — app_payload is recv-v2 only.
                let raw_len = msg.as_slice().len();
                frame.set_ok(sender, raw_len, frame.ret2());
                crate::yarm_log!(
                    "IPC_RECV_WAKE_RETURN_REGS tid={} x0={} x1={} x2={} elr=na",
                    receiver_tid,
                    sender,
                    msg.len,
                    frame.ret2()
                );
                let words =
                    pack_register_payload(msg.as_slice()).map_err(|_| SyscallError::InvalidArgs)?;
                frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD0, words[0]);
                frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD1, words[1]);
            }
        }
        None => {
            frame.set_err(empty_error.code());
            encode_transfer_cap_ret(frame, None)?;
        }
    }
    Ok(())
}
