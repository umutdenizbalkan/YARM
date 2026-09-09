// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-XFER2 §3 — the SPLIT adapter for NR 30 `RecvSharedV3`.
//!
//! One method per domain acquisition, driving the same
//! [`crate::kernel::syscall::recv_v3_txn`] policy the broad adapter drives.
//!
//! The user-memory owners are the ones that already existed and already have live callers:
//! `copy_from_user_split` and `copy_to_user_split`, documented in `user_memory_state.rs` as
//! rank-5/6-seam mirrors of `KernelState::copy_from_user` / `copy_to_user` with identical error
//! semantics. Nothing new copies bytes here.

use crate::kernel::capabilities::{CapId, CapObject};
use crate::kernel::ipc::{Message, SenderWakeTarget, ThreadId};
use crate::kernel::syscall::SyscallError;
use crate::kernel::syscall::recv_v3_txn::{
    PeekedHead, RecvV3Owners, V3CommitOutcome, V3ObjectMeta, V3TxnEvent,
};
use crate::kernel::vm::{Asid, PhysAddr, VirtAddr};

/// The split adapter. `tid` is the requester the trap seam resolved; `cpu` is the CPU the trap
/// actually entered on, so the unmap reaches the requester-stating shootdown owner.
pub(crate) struct SplitRecvV3Owners<'a> {
    pub(crate) shared: &'a crate::runtime::SharedKernel,
    pub(crate) tid: u64,
    pub(crate) cpu: crate::kernel::scheduler::CpuId,
}

impl RecvV3Owners for SplitRecvV3Owners<'_> {
    fn caller_tid(&mut self) -> u64 {
        self.tid
    }

    fn caller_asid(&mut self) -> Option<Asid> {
        self.shared.task_asid_option_split_read(self.tid)
    }

    fn read_user(&mut self, asid: Asid, ptr: usize, len: usize) -> Option<[u8; 80]> {
        // `copy_from_user_split` is the rank-5/6 mirror of `KernelState::copy_from_user`, capped
        // at `Message::MAX_PAYLOAD` (128) — comfortably above NR 30's 80-byte request record.
        let wide = self
            .shared
            .copy_from_user_split(asid, VirtAddr(ptr as u64), len)
            .ok()?;
        let mut out = [0u8; 80];
        let take = len.min(80);
        out[..take].copy_from_slice(&wide[..take]);
        Some(out)
    }

    fn write_user(&mut self, asid: Asid, ptr: usize, bytes: &[u8]) -> bool {
        self.shared
            .copy_to_user_split(asid, VirtAddr(ptr as u64), bytes)
            .is_ok()
    }

    fn resolve_recv_endpoint(&mut self, cap: CapId) -> Result<CapObject, SyscallError> {
        use crate::kernel::capabilities::CapRights;
        let capability = self
            .shared
            .resolve_capability_for_task_split(self.tid, cap)
            .map_err(SyscallError::from)?;
        if !capability.has_right(CapRights::RECEIVE) {
            return Err(SyscallError::MissingRight);
        }
        // Liveness, exactly as the broad pair checks it.
        if self
            .shared
            .capability_object_live_split(capability.object)
            .is_none()
        {
            return Err(SyscallError::InvalidCapability);
        }
        Ok(capability.object)
    }

    fn endpoint_index(&mut self, endpoint: CapObject) -> Result<usize, SyscallError> {
        match endpoint {
            CapObject::Endpoint { index, .. } => Ok(index),
            _ => Err(SyscallError::WrongObject),
        }
    }

    fn peek_head(&mut self, endpoint_idx: usize) -> Result<Option<Message>, SyscallError> {
        use crate::kernel::boot::IpcEndpointPeekResult;
        Ok(
            match self
                .shared
                .peek_queued_with_cap_transfer_split(endpoint_idx)
            {
                IpcEndpointPeekResult::Peeked(msg) => Some(msg),
                IpcEndpointPeekResult::Ineligible(_) => None,
            },
        )
    }

    fn materialize_cap(
        &mut self,
        endpoint: CapObject,
        sender_tid: u64,
        msg: &Message,
    ) -> Result<Option<u64>, SyscallError> {
        self.shared
            .materialize_received_message_cap_split(endpoint, self.tid, sender_tid, msg)
    }

    fn object_meta(&mut self, cap: CapId) -> V3ObjectMeta {
        let Ok(capability) = self.shared.resolve_capability_for_task_split(self.tid, cap) else {
            return V3ObjectMeta::default();
        };
        V3ObjectMeta {
            kind: crate::kernel::syscall::recv_shared_v3::recv_v3_object_kind(capability.object),
            generation: crate::kernel::syscall::recv_shared_v3::recv_v3_object_generation(
                capability.object,
            ),
            effective_rights: u32::from(capability.rights_bits()),
            exact_object_size: self.shared.memory_object_len_split(capability.object),
            exact_region_len: crate::kernel::syscall::recv_shared_v3::recv_v3_exact_region_len(
                capability.object,
            ),
        }
    }

    fn region_phys_start(&mut self, cap: CapId) -> Option<PhysAddr> {
        let capability = self
            .shared
            .resolve_capability_for_task_split(self.tid, cap)
            .ok()?;
        let (mo_id, offset) = match capability.object {
            CapObject::DmaRegion { id, offset, .. } => (id, offset),
            CapObject::MemoryObject { id } => (id, 0u64),
            _ => return None,
        };
        self.shared
            .memory_object_phys_by_id_split(mo_id)
            .map(|phys| PhysAddr(phys.0 + offset))
    }

    fn map_page(&mut self, asid: Asid, virt: VirtAddr, phys: PhysAddr, writable: bool) -> bool {
        use crate::kernel::vm::{CachePolicy, Mapping, PageFlags};
        self.shared.map_user_page_raw_split(
            asid,
            virt,
            Mapping {
                phys,
                flags: PageFlags {
                    read: true,
                    write: writable,
                    execute: false,
                    user: true,
                    cache_policy: CachePolicy::WriteBack,
                },
            },
        )
    }

    fn unmap_range(&mut self, asid: Asid, base: usize, len: usize) {
        // Requester-stating, so the shootdown excludes the CPU this trap entered on rather than
        // whichever CPU last wrote the ambient field (U9-VM-ENTRY1-S §1).
        let _ = self
            .shared
            .unmap_range_two_phase_from_split(self.cpu, asid, base, len);
    }

    fn register_transfer(&mut self, cap: CapId, base: VirtAddr, len: usize) -> bool {
        self.shared
            .register_active_transfer_mapping_split(ThreadId(self.tid), cap, base, len)
    }

    fn remove_transfer(&mut self, cap: CapId) -> bool {
        self.shared
            .remove_active_transfer_mapping_split(ThreadId(self.tid), cap)
    }

    fn commit_peeked(&mut self, head: &PeekedHead) -> V3CommitOutcome {
        use crate::kernel::boot::IpcEndpointRecvResult;
        match self
            .shared
            .commit_peeked_recv_with_cap_transfer_split(head.endpoint_idx, &head.msg)
        {
            IpcEndpointRecvResult::Received(_) => V3CommitOutcome::Consumed { wake: None },
            IpcEndpointRecvResult::ReceivedWithSenderWake(_, wake) => {
                V3CommitOutcome::Consumed { wake: Some(wake) }
            }
            IpcEndpointRecvResult::Ineligible(_) => V3CommitOutcome::LostRace,
        }
    }

    fn settle_sender(&mut self, wake: SenderWakeTarget) {
        // The EXISTING off-lock settle: the same marker, the same U6 completion publication
        // before the wake, the same rank-2 → rank-1 body. Nothing new settles a sender.
        let _ = self
            .shared
            .apply_split_sender_wake_plan_split(self.cpu, wake);
    }

    fn rollback_cap(&mut self, cap: CapId, is_reply: bool) {
        if is_reply {
            // U9-C owns the reply-registry transaction; the ordinary composition declines it.
            let _ = self
                .shared
                .rollback_materialized_recv_cap_no_vm_split(self.tid, cap);
            return;
        }
        let _ = self
            .shared
            .rollback_materialized_recv_cap_no_vm_split(self.tid, cap);
    }

    fn note(&mut self, event: V3TxnEvent) {
        crate::kernel::syscall::recv_shared_v3::note_v3_event("split", event);
    }
}
