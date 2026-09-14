// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-RECV-QUEUE1 §2 — the receiver side of a queued shared-region transfer, off the broad lock.
//!
//! A `send_shared_region` enqueues an `OPCODE_SHARED_MEM` message whose transfer envelope carries
//! the `+1` MemoryObject pin. Receiving one is not a payload copy: the receiver names a virtual
//! base, the kernel mints the transferred capability, maps the region there, registers an
//! active-transfer entry so the eventual release can find it, and reports the mapping in the
//! frame's argument registers. Until this module existed the split receive engine declined the
//! whole class before the dequeue, and every such message reached the terminal acquisition.
//!
//! # Which transaction this is, and which it is not
//!
//! The order here is **NR 2 / NR 5's**, taken from
//! `handle_ipc_recv_result_with_empty_error`'s shared-region arm step for step: materialize,
//! publish the cap in the return lane, write the recv-v2 metadata if one was asked for, then
//! decode, validate, attenuate, map, register, and write the frame. Failures compensate by
//! revoking the just-minted capability and clearing the return lane — the same compensation, at
//! the same steps, with the same errors.
//!
//! It is deliberately **not** NR 30's. `RecvSharedV3` peeks the head, builds everything it can
//! fail at, and only then commits the dequeue, which is a better transaction; NR 2 and NR 5
//! consume first and compensate, and the point of this module is that the split route behaves
//! the way the broad route behaves, not the way a different syscall does. Nothing of NR 30's ABI
//! — its 80-byte request record, its output record, its `V3ObjectMeta` — appears here.
//!
//! # Two canonical quirks, reproduced rather than repaired
//!
//! * **The map-intent word shares a register with the metadata slots.**
//!   `recv_shared_mem_map_intent_flags` reads `SYSCALL_ARG_INLINE_PAYLOAD1` (arg 4) straight off
//!   the frame; arg 4 is NR 2's recv-v2 metadata LENGTH and NR 5's metadata POINTER. That reader
//!   is imported and called, not re-derived, so the split route cannot drift from it.
//! * **The metadata struct's cap field is read before attenuation can change it.** The broad arm
//!   encodes `frame.ret2()` into the metadata *before* the read-only attenuation may mint a
//!   different capability and rewrite the return lane. The metadata therefore names the
//!   pre-attenuation capability. Same order here, same bytes.
//!
//! A third is noted where it happens: the per-page mapping loop resolves the memory object's
//! physical base once per page from the capability, which is the object's base every time. For a
//! region of one page — the only shape production sends — that is exactly right; the broad owner
//! has the same shape and this module reproduces it rather than silently diverging.

use crate::kernel::boot::TrapHandleError;
use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
use crate::kernel::ipc::{SharedMemoryRegion, ThreadId};
use crate::kernel::recv_core::RecvBoundarySharedRegionSnapshot;
use crate::kernel::scheduler::CpuId;
use crate::kernel::syscall::{
    IPC_RECV_META_V2_ENCODED_LEN, SYSCALL_ARG_INLINE_PAYLOAD0, SYSCALL_ARG_INLINE_PAYLOAD1,
    SYSCALL_NO_TRANSFER_CAP, SYSCALL_RECV_META_TRANSFERRED_CAP, SyscallError,
    recv_boundary_encode_transfer_cap_ret, recv_shared_mem_map_intent_flags, round_up_page,
};
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::{Asid, Mapping, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};

/// Where the recv-v2 metadata lives for the syscall that is actually in the frame.
///
/// NR 2 puts `(ptr, len)` in args 3/4; NR 5 puts them in args 4/5, because arg 3 is its timeout.
/// The broad result owner is handed the right pair by its caller; here the frame is all there is,
/// so the syscall number picks the pair — and it picks it ONCE, in one place.
fn recv_v2_meta_slots(frame: &TrapFrame) -> Option<(usize, usize)> {
    use crate::kernel::syscall::{SYSCALL_ARG_TRANSFER_CAP, Syscall};
    let (ptr, len) = match Syscall::decode(frame.syscall_num()) {
        Ok(Syscall::IpcRecv) => (
            frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
            frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
        ),
        Ok(Syscall::IpcRecvTimeout) => (
            frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
            frame.arg(SYSCALL_ARG_TRANSFER_CAP),
        ),
        _ => return None,
    };
    (ptr != 0 && len >= IPC_RECV_META_V2_ENCODED_LEN).then_some((ptr, len))
}

impl crate::runtime::SharedKernel {
    /// The receiver-side transaction for a queued `OPCODE_SHARED_MEM` message, in the order
    /// `handle_ipc_recv_result_with_empty_error` runs it.
    ///
    /// On entry the message is consumed and the sender wake decided but not applied; nothing else
    /// has happened. On every exit the envelope is settled, the sender is woken exactly once, and
    /// either the region is mapped and the frame written or the minted capability has been
    /// revoked and the return lane cleared.
    pub(crate) fn complete_recv_boundary_shared_region(
        &self,
        cpu: CpuId,
        frame: &mut TrapFrame,
        pending: &RecvBoundarySharedRegionSnapshot,
    ) -> Result<(), TrapHandleError> {
        let receiver_tid = pending.receiver_tid;
        crate::yarm_log!(
            "IPC_RECV_SHARED_REGION_SPLIT_BEGIN cpu={} receiver_tid={} opcode={}",
            cpu.0,
            receiver_tid,
            pending.msg.opcode
        );

        // ── step 1: materialize, through the queued-envelope materializer ───────────────────
        //
        // `materialize_queued_transfer_cap_split` is the owner that services a shared-region
        // envelope: it consumes the envelope exactly once at rank 3, REPORTS the pin obligation
        // rather than performing it, mints at rank 4, and releases the pin at rank 6 on every
        // exit. That is the same decision the broad `take_transfer_envelope` makes in one step.
        let Some(raw_handle) = pending.msg.transferred_cap().map(|c| c.0) else {
            // A shared-region message with no transfer envelope never reaches the broad arm's
            // mapping either: `frame.ret2()` stays the no-transfer sentinel and the arm answers
            // `InvalidArgs`. Same answer, one step earlier, nothing consumed but the message.
            self.settle_shared_region_sender(cpu, pending);
            return Err(TrapHandleError::Syscall(SyscallError::InvalidArgs));
        };
        let endpoint_idx = match pending.endpoint {
            CapObject::Endpoint { index, .. } => index,
            _ => {
                self.settle_shared_region_sender(cpu, pending);
                return Err(TrapHandleError::Syscall(SyscallError::WrongObject));
            }
        };
        let transfer_cap = match self.materialize_queued_transfer_cap_split(
            endpoint_idx,
            receiver_tid,
            raw_handle,
        ) {
            Ok(cap) => cap,
            Err(e) => {
                crate::yarm_log!(
                    "IPC_RECV_CAP_MATERIALIZE_FAILED kind=shared_region raw={} err={:?}",
                    raw_handle,
                    e
                );
                self.settle_shared_region_sender(cpu, pending);
                return Err(TrapHandleError::Syscall(e));
            }
        };
        crate::yarm_log!(
            "IPC_RECV_IMMEDIATE_TRANSFER_CAP_MINT tid={} local_cap={} raw={}",
            receiver_tid,
            transfer_cap.0,
            raw_handle
        );

        // ── step 2: publish the receiver-local CapId in the return lane ─────────────────────
        if recv_boundary_encode_transfer_cap_ret(frame, Some(transfer_cap.0)).is_err() {
            self.revoke_shared_region_transfer_cap(receiver_tid, transfer_cap);
            self.settle_shared_region_sender(cpu, pending);
            return Err(TrapHandleError::Syscall(SyscallError::Internal));
        }

        // ── step 3: the recv-v2 metadata struct, if the caller asked for one ────────────────
        //
        // Written BEFORE the mapping, exactly as the broad arm writes it, so its capability
        // field names the pre-attenuation capability. A copy fault rolls the mint back and
        // clears the return lane through the established rollback owner.
        if let Some((meta_ptr, _meta_len)) = recv_v2_meta_slots(frame) {
            let delivery =
                crate::kernel::syscall::ipc_recv_core::project_recv_delivery(&pending.msg);
            let sender = match usize::try_from(pending.msg.sender_tid.0) {
                Ok(s) => s,
                Err(_) => {
                    self.revoke_shared_region_transfer_cap(receiver_tid, transfer_cap);
                    self.settle_shared_region_sender(cpu, pending);
                    return Err(TrapHandleError::Syscall(SyscallError::Internal));
                }
            };
            // `SYSCALL_RECV_META_TRANSFERRED_CAP` and not the reply-cap bit: this arm is only
            // reached for a non-reply transfer, which is the same branch the broad arm takes.
            let meta = crate::kernel::syscall::ipc_recv_core::encode_recv_v2_meta(
                sender as u64,
                delivery.app_opcode,
                pending.msg.flags,
                delivery.app_payload.len() as u32,
                frame.ret2() as u64,
                SYSCALL_RECV_META_TRANSFERRED_CAP as u64,
                pending.msg.sender_tid.0,
            );
            let Some(asid) = pending.asid else {
                self.revoke_shared_region_transfer_cap(receiver_tid, transfer_cap);
                self.settle_shared_region_sender(cpu, pending);
                return Err(TrapHandleError::Syscall(SyscallError::from(
                    crate::kernel::boot::KernelError::UserMemoryFault,
                )));
            };
            if let Err(copy_err) = self.copy_to_user_split(asid, VirtAddr(meta_ptr as u64), &meta) {
                self.revoke_shared_region_transfer_cap(receiver_tid, transfer_cap);
                let _ = recv_boundary_encode_transfer_cap_ret(frame, None);
                crate::yarm_log!("IPC_RECV_V2_ROLLBACK_OK site=immediate_meta reply=false");
                self.settle_shared_region_sender(cpu, pending);
                return Err(TrapHandleError::Syscall(SyscallError::from(copy_err)));
            }
            crate::yarm_log!("IPC_RECV_V2_META_IMMEDIATE_OK len=40");
        }

        // ── step 4: the sender wake, after the mint and before any writeback (§56 order) ────
        self.settle_shared_region_sender(cpu, pending);

        // ── steps 5-13: decode, validate, attenuate, map, register, write the frame ─────────
        match self.install_shared_region_for_receiver(cpu, frame, pending, transfer_cap) {
            Ok(()) => {
                crate::yarm_log!(
                    "IPC_RECV_SHARED_REGION_SPLIT_DONE cpu={} receiver_tid={} result=ok",
                    cpu.0,
                    receiver_tid
                );
                Ok(())
            }
            Err(e) => {
                crate::yarm_log!(
                    "IPC_RECV_SHARED_REGION_SPLIT_DONE cpu={} receiver_tid={} result=err err={:?}",
                    cpu.0,
                    receiver_tid,
                    e
                );
                Err(TrapHandleError::Syscall(e))
            }
        }
    }

    /// The mapping half, from `SharedMemoryRegion::decode` to the frame writeback.
    ///
    /// Split out so the compensation ladder reads as one sequence: every failure below revokes
    /// the transfer capability and clears the return lane exactly where the broad arm does, and
    /// the two that do NOT revoke — a missing transfer capability in the return lane, and a
    /// capability that no longer resolves — do not revoke here either.
    fn install_shared_region_for_receiver(
        &self,
        cpu: CpuId,
        frame: &mut TrapFrame,
        pending: &RecvBoundarySharedRegionSnapshot,
        minted: CapId,
    ) -> Result<(), SyscallError> {
        let receiver_tid = pending.receiver_tid;

        // (5) the descriptor, and (6) its length.
        let desc =
            SharedMemoryRegion::decode(pending.msg.as_slice()).ok_or(SyscallError::InvalidArgs)?;
        let region_len = usize::try_from(desc.len).map_err(|_| SyscallError::InvalidArgs)?;

        // (7) the receiver's buffer must exist and be big enough. Revokes.
        if pending.user_ptr == 0 || pending.user_len < region_len {
            if frame.ret2() as u64 != SYSCALL_NO_TRANSFER_CAP {
                self.revoke_shared_region_transfer_cap(receiver_tid, minted);
                recv_boundary_encode_transfer_cap_ret(frame, None)
                    .map_err(|_| SyscallError::Internal)?;
            }
            return Err(SyscallError::InvalidArgs);
        }

        // (8) the return lane must name a capability. Does NOT revoke — there is nothing to
        // revoke when the lane carries the sentinel.
        let transfer_cap_raw =
            u64::try_from(frame.ret2()).map_err(|_| SyscallError::InvalidArgs)?;
        if transfer_cap_raw == SYSCALL_NO_TRANSFER_CAP {
            return Err(SyscallError::InvalidArgs);
        }
        let mut transfer_cap = CapId(transfer_cap_raw);

        // (9) the map intent, through the canonical frame reader. Revokes.
        let recv_map_flags = match recv_shared_mem_map_intent_flags(frame) {
            Ok(flags) => flags,
            Err(err) => {
                self.revoke_shared_region_transfer_cap(receiver_tid, transfer_cap);
                recv_boundary_encode_transfer_cap_ret(frame, None)
                    .map_err(|_| SyscallError::Internal)?;
                return Err(err);
            }
        };

        // (10) resolve the capability in the RECEIVER's cspace. Does NOT revoke.
        let transfer_capability = self
            .resolve_capability_for_task_split(receiver_tid, transfer_cap)
            .map_err(|_| SyscallError::InvalidCapability)?;

        // (11) a write mapping needs WRITE on the capability. Revokes.
        if recv_map_flags.write && !transfer_capability.has_right(CapRights::WRITE) {
            self.revoke_shared_region_transfer_cap(receiver_tid, transfer_cap);
            recv_boundary_encode_transfer_cap_ret(frame, None)
                .map_err(|_| SyscallError::Internal)?;
            return Err(SyscallError::MissingRight);
        }

        // (12) read-only intent attenuates the capability. `?`, no revoke — matching the broad
        // arm, whose `attenuate_transfer_cap_for_recv_intent(...)?` also propagates bare.
        let attenuated = self.attenuate_shared_region_cap_split(
            receiver_tid,
            transfer_cap,
            recv_map_flags.write,
        )?;
        if attenuated.0 != transfer_cap.0 {
            transfer_cap = attenuated;
            recv_boundary_encode_transfer_cap_ret(frame, Some(transfer_cap.0))
                .map_err(|_| SyscallError::Internal)?;
        }

        // (13) map the region. Revokes on failure, after rolling back its own partial pages.
        let Some(asid) = pending.asid else {
            return Err(SyscallError::from(
                crate::kernel::boot::KernelError::UserMemoryFault,
            ));
        };
        let (mapped_va, mapped_len) = match self.map_shared_region_into_receiver_split(
            cpu,
            asid,
            receiver_tid,
            transfer_cap,
            pending.user_ptr,
            region_len,
            recv_map_flags,
        ) {
            Ok(mapped) => mapped,
            Err(err) => {
                self.revoke_shared_region_transfer_cap(receiver_tid, transfer_cap);
                recv_boundary_encode_transfer_cap_ret(frame, None)
                    .map_err(|_| SyscallError::Internal)?;
                return Err(err);
            }
        };

        // (14/15) register the active transfer. On failure: unmap what was installed through the
        // two-phase owner (shootdown completes before any frame is reclaimed), then revoke.
        if !self.register_active_transfer_mapping_split(
            ThreadId(receiver_tid),
            transfer_cap,
            VirtAddr(mapped_va as u64),
            mapped_len,
        ) {
            let _ = self.unmap_range_two_phase_from_split(cpu, asid, mapped_va, mapped_len);
            self.revoke_shared_region_transfer_cap(receiver_tid, transfer_cap);
            let _ = recv_boundary_encode_transfer_cap_ret(frame, None);
            // `EndpointFull` is what the broad `register_active_transfer_mapping` raises when
            // the registry has no free slot, and the broad arm forwards it through
            // `SyscallError::from(e)`. The locked body the split seam drives returns a bare
            // `bool`, so the error is restated here rather than re-derived from a new condition.
            return Err(SyscallError::from(
                crate::kernel::boot::KernelError::EndpointFull,
            ));
        }

        // (16/17) accounting, then the frame: `ret0 = 0`, `ret1 = mapped_len`, `ret2` unchanged,
        // and the mapping's base and the region's byte length in the inline argument registers.
        self.note_shared_mem_mapped_split(mapped_len);
        frame.set_ok(0, mapped_len, frame.ret2());
        frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD0, mapped_va);
        frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD1, region_len);
        crate::yarm_log!(
            "IPC_RECV_SHARED_REGION_SPLIT_MAPPED receiver_tid={} va=0x{:x} mapped_len={} region_len={} cap={}",
            receiver_tid,
            mapped_va,
            mapped_len,
            region_len,
            transfer_cap.0
        );
        Ok(())
    }

    /// The deferred sender wake, through the EXISTING off-lock settle — the same owner, the same
    /// U6 completion publication before the wake, the same marker. Applied exactly once per
    /// delivery, on every exit path.
    fn settle_shared_region_sender(&self, cpu: CpuId, pending: &RecvBoundarySharedRegionSnapshot) {
        if let Some(wake_tid) = pending.wake_tid {
            let _ = self.apply_split_sender_wake_plan_split(cpu, wake_tid);
            crate::yarm_log!(
                "IPC_RECV_V2_SENDER_WAKE_ORDER_OK wake_tid={} phase=before_writeback",
                wake_tid.tid.0
            );
        }
    }

    /// The split twin of `revoke_current_transfer_cap_best_effort`: best-effort, and scoped to
    /// the RECEIVER's cnode rather than to whichever task happens to be current.
    ///
    /// `revoke_capability_no_vm_split` is the complete off-broad-lock teardown, performing the
    /// same steps in the same order as `revoke_capability_in_cnode` — the in-cspace derivation
    /// revoke, the delegated descendants, the delegation links, the active-transfer-mapping
    /// revocation, the memory refcount drop and reclaim, and the notification destroy. Its
    /// result is discarded, exactly as the broad helper discards its own.
    fn revoke_shared_region_transfer_cap(&self, receiver_tid: u64, transfer_cap: CapId) {
        let _ = self.revoke_capability_no_vm_split(receiver_tid, transfer_cap);
    }

    /// The split twin of `attenuate_transfer_cap_for_recv_intent`, with the same three outcomes
    /// in the same order: a write mapping keeps the capability as it is; an already-read-only
    /// capability with `READ | MAP` is kept as it is; anything else is re-minted with the
    /// intersection and the original revoked.
    fn attenuate_shared_region_cap_split(
        &self,
        receiver_tid: u64,
        transfer_cap: CapId,
        allow_write: bool,
    ) -> Result<CapId, SyscallError> {
        if allow_write {
            return Ok(transfer_cap);
        }
        let capability = self
            .resolve_capability_for_task_split(receiver_tid, transfer_cap)
            .map_err(|_| SyscallError::InvalidCapability)?;
        let desired = CapRights::READ | CapRights::MAP;
        if capability.rights().contains(desired) && !capability.rights().contains(CapRights::WRITE)
        {
            return Ok(transfer_cap);
        }
        let attenuated_rights = capability.rights().intersect(desired);
        let cnode = self
            .task_cnode_split(receiver_tid)
            .ok_or(SyscallError::InvalidCapability)?;
        // The broad helper mints into the CURRENT context; the receiver IS the current task on
        // this path (it is the one that trapped), so the same cnode is named — explicitly,
        // rather than by relying on an ambient reader this route has deliberately removed.
        let derived = self
            .sr_mint_split(cnode, Capability::new(capability.object, attenuated_rights))
            .map_err(|()| SyscallError::from(crate::kernel::boot::KernelError::CapabilityFull))?;
        self.revoke_shared_region_transfer_cap(receiver_tid, transfer_cap);
        Ok(derived)
    }

    /// The split twin of `map_shared_region_into_receiver`: the same alignment and overflow
    /// gates, the same page loop, and the same two-phase rollback of the pages it installed
    /// before a failure — reclaim only after the shootdown.
    #[allow(clippy::too_many_arguments)]
    fn map_shared_region_into_receiver_split(
        &self,
        cpu: CpuId,
        asid: Asid,
        receiver_tid: u64,
        receiver_mem_cap: CapId,
        requested_va: usize,
        region_len: usize,
        map_flags: PageFlags,
    ) -> Result<(usize, usize), SyscallError> {
        if requested_va == 0 || region_len == 0 || !requested_va.is_multiple_of(PAGE_SIZE) {
            return Err(SyscallError::InvalidArgs);
        }
        let mapped_len = round_up_page(region_len)?;
        let end = requested_va
            .checked_add(mapped_len)
            .ok_or(SyscallError::InvalidArgs)?;
        let mut va = requested_va;
        while va < end {
            // Resolved per page, as the broad loop resolves it per page through
            // `map_user_page_in_asid_with_caps` → `resolve_memory_object_phys`: the rights check
            // and the object's physical base, in that order, with that error precedence.
            let phys =
                self.resolve_shared_region_phys_split(receiver_tid, receiver_mem_cap, map_flags)?;
            let installed = self.map_user_page_raw_split(
                asid,
                VirtAddr(va as u64),
                Mapping {
                    phys,
                    flags: PageFlags {
                        read: map_flags.read,
                        write: map_flags.write,
                        execute: map_flags.execute,
                        user: map_flags.user,
                        cache_policy: map_flags.cache_policy,
                    },
                },
            );
            if !installed {
                // Two-phase rollback of the pages already installed by THIS loop, requester-
                // stating so the shootdown excludes the CPU this trap entered on.
                if va > requested_va {
                    let _ = self.unmap_range_two_phase_from_split(
                        cpu,
                        asid,
                        requested_va,
                        va - requested_va,
                    );
                }
                return Err(SyscallError::from(
                    crate::kernel::boot::KernelError::UserMemoryFault,
                ));
            }
            va += PAGE_SIZE;
        }
        Ok((requested_va, mapped_len))
    }

    /// The split twin of `resolve_memory_object_phys`, scoped to the receiver's cspace: resolve,
    /// require the object kind, check READ then WRITE against the requested flags, then read the
    /// object's physical base. Same order, same errors.
    fn resolve_shared_region_phys_split(
        &self,
        receiver_tid: u64,
        mem_cap: CapId,
        flags: PageFlags,
    ) -> Result<PhysAddr, SyscallError> {
        let capability = self
            .resolve_capability_for_task_split(receiver_tid, mem_cap)
            .map_err(|_| SyscallError::InvalidCapability)?;
        let id = match capability.object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return Err(SyscallError::WrongObject),
        };
        if flags.read && !capability.has_right(CapRights::READ) {
            return Err(SyscallError::MissingRight);
        }
        if flags.write && !capability.has_right(CapRights::WRITE) {
            return Err(SyscallError::MissingRight);
        }
        self.memory_object_phys_by_id_split(id)
            .ok_or(SyscallError::from(
                crate::kernel::boot::KernelError::MemoryObjectMissing,
            ))
    }

    /// rank 3 — the mapping accounting, the split twin of `KernelState::note_shared_mem_mapped`:
    /// the one counter that method bumps, with the same saturating arithmetic. Sibling of the
    /// already-existing `note_shared_mem_released_split`, which bumps two because
    /// `note_shared_mem_released` does.
    fn note_shared_mem_mapped_split(&self, len: usize) {
        self.with_ipc_split_mut(|ipc| {
            ipc.telemetry.shared_mem_bytes_mapped = ipc
                .telemetry
                .shared_mem_bytes_mapped
                .saturating_add(len as u64);
        });
    }
}
