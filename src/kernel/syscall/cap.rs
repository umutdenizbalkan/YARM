// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Capability-domain syscall handlers.
//!
//! D4 step 4: mechanically split from the parent `syscall.rs` module with zero
//! behavior change. `syscall.rs` keeps minimal delegation shims so dispatch
//! routing remains explicit while capability/CNode semantics stay owned by the
//! existing `KernelState` capability methods.

use super::{
    SYSCALL_ARG_CAP, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR, SyscallError, current_task_has_user_asid,
    current_tid,
};
use crate::kernel::boot::{ControlPlaneCnodePlan, KernelError, KernelState};
use crate::kernel::capabilities::CapId;
use crate::kernel::ipc::ThreadId;
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::VirtAddr;

/// U9-XFER1 §3 — the BROAD adapter for NR 4 `TransferRelease`.
///
/// Holds no policy of its own: it resolves the owners out of `&mut KernelState` and runs the same
/// [`crate::kernel::syscall::xfer_txn::run_transfer_release_transaction`] the split adapter runs.
/// Every validation, ordering and refusal decision lives once, in that module.
pub(crate) struct BroadXferOwners<'a> {
    pub(crate) kernel: &'a mut KernelState,
}

/// U9-XFER2 §2 — the broad route's revoke reservation.
///
/// Identity, consumed by the commit. See `reserve_capability_release` for why capacity is not
/// carried here and what that does and does not claim.
pub(crate) struct BroadXferReservation {
    pub(crate) cnode: crate::kernel::capabilities::CNodeId,
    pub(crate) cap: CapId,
}

impl crate::kernel::syscall::xfer_txn::XferReleaseOwners for BroadXferOwners<'_> {
    type RevokeReservation = BroadXferReservation;

    fn caller_with_user_asid(&mut self) -> Option<(ThreadId, crate::kernel::vm::Asid)> {
        if !current_task_has_user_asid(self.kernel).ok()? {
            return None;
        }
        let owner = ThreadId(current_tid(self.kernel).ok()?);
        // Stage 7's note holds: `current_task_has_user_asid` succeeding is exactly what makes
        // `task_asid` return `Some`, so the two are resolved as one owner rather than as a check
        // followed by an unreachable error arm.
        let asid = self.kernel.task_asid(owner.0)?;
        Some((owner, asid))
    }

    fn registered_range(&mut self, owner: ThreadId, cap: CapId) -> Option<(VirtAddr, usize)> {
        self.kernel.active_transfer_mapping_for(owner, cap)
    }

    fn caller_cnode(&mut self) -> Option<crate::kernel::capabilities::CNodeId> {
        self.kernel.current_task_cnode()
    }

    fn page_is_mapped(&mut self, asid: crate::kernel::vm::Asid, virt: VirtAddr) -> bool {
        self.kernel
            .is_user_page_mapped_in_asid(asid, virt)
            .unwrap_or(false)
    }

    fn reserve_capability_release(
        &mut self,
        cnode: crate::kernel::capabilities::CNodeId,
        cap: CapId,
    ) -> Option<Self::RevokeReservation> {
        // What a reservation MEANS differs between the two routes, and the difference is
        // structural rather than an omission here.
        //
        // The split route releases every domain lock between phases, so its reservation must
        // carry owned capacity across that window: an allocation attempted after the range was
        // unmapped would be a failure with nowhere to go. The broad route holds ONE acquisition
        // across the whole transaction, so no window exists — the closure `revoke_capability_in_cnode`
        // builds is allocated inside the same critical section that performs the unmap, and
        // hoisting it here would change nothing about when it could fail.
        //
        // So the broad reservation carries IDENTITY only, and the commit consumes exactly that.
        // What is NOT claimed: this does not make a broad allocation failure a pre-mutation
        // refusal. That behaviour belongs to `collect_delegated_descendants`, which grows with
        // `push`, and it is unchanged by this transaction.
        self.kernel
            .capability_for_cnode_local(cnode, cap)
            .map(|_| BroadXferReservation { cnode, cap })
    }

    fn unmap_whole_range(
        &mut self,
        asid: crate::kernel::vm::Asid,
        base: usize,
        map_len: usize,
    ) -> bool {
        // The existing broad two-phase unmap: per page, remove the PTE, complete the shootdown,
        // then reclaim. Its per-page errors are swallowed by design — the transaction's phase V
        // already proved every page is mapped, so the `Ok(None)` case this used to refuse on
        // cannot arise here.
        self.kernel.unmap_range_two_phase(asid, base, map_len);
        true
    }

    fn revoke_reserved_capability(
        &mut self,
        reservation: Self::RevokeReservation,
        backing_is_quarantined: bool,
    ) -> crate::kernel::syscall::xfer_txn::XferRevokeOutcome {
        use crate::kernel::syscall::xfer_txn::XferRevokeOutcome;
        // The broad route runs the unmap and the revoke inside ONE acquisition, so an incomplete
        // shootdown cannot be followed by a separate reclaimer racing in between — but the
        // reclaim itself would still free a frame whose translation was not retired. The verdict
        // is honoured the same way it is on the split route: by not reclaiming.
        if backing_is_quarantined {
            crate::yarm_log!(
                "XFER_REVOKE_RECLAIM_QUARANTINED route=broad cap={} reason=caller_shootdown_incomplete",
                reservation.cap.0
            );
        }
        match self
            .kernel
            .revoke_capability_in_cnode(reservation.cnode, reservation.cap)
        {
            Ok(()) => XferRevokeOutcome::Revoked,
            Err(_) => XferRevokeOutcome::AlreadyRetired,
        }
    }

    fn remove_registration(&mut self, owner: ThreadId, cap: CapId) -> bool {
        self.kernel.remove_active_transfer_mapping(owner, cap)
    }

    fn account_release(&mut self, map_len: usize) {
        self.kernel.note_shared_mem_released(map_len);
    }

    fn note(&mut self, event: crate::kernel::syscall::xfer_txn::XferTxnEvent) {
        note_xfer_event("broad", event);
    }
}

/// The shared marker text for both adapters, so a live capture cannot tell them apart by accident
/// and a reader can tell them apart on purpose.
pub(crate) fn note_xfer_event(route: &str, event: crate::kernel::syscall::xfer_txn::XferTxnEvent) {
    use crate::kernel::syscall::xfer_txn::XferTxnEvent;
    match event {
        XferTxnEvent::Refused { reason } => {
            crate::yarm_log!("XFER_RELEASE_REFUSED route={} reason={:?}", route, reason);
        }
        XferTxnEvent::Released { pages, map_len } => {
            crate::yarm_log!(
                "XFER_RELEASE_OK route={} pages={} len={}",
                route,
                pages,
                map_len
            );
        }
        XferTxnEvent::ReleasedWithIncompleteShootdown { pages, map_len } => {
            crate::yarm_log!(
                "XFER_RELEASE_SHOOTDOWN_INCOMPLETE route={} pages={} len={}",
                route,
                pages,
                map_len
            );
        }
        XferTxnEvent::ReleasedAfterConcurrentRevoke { pages, map_len } => {
            crate::yarm_log!(
                "XFER_RELEASE_RACED_REVOKE route={} pages={} len={}",
                route,
                pages,
                map_len
            );
        }
    }
}

pub(super) fn handle_transfer_release(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let transfer_cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let base_arg = frame.arg(SYSCALL_ARG_PTR);
    let len_arg = frame.arg(SYSCALL_ARG_LEN);
    let mut owners = BroadXferOwners { kernel };
    let map_len = crate::kernel::syscall::xfer_txn::run_transfer_release_transaction(
        &mut owners,
        transfer_cap,
        base_arg,
        len_arg,
    )?;
    frame.set_ok(map_len, 0, 0);
    Ok(())
}

pub(super) fn handle_control_plane_set_cnode_slots(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let requester_tid = current_tid(kernel)?;
    let target_pid = frame.arg(SYSCALL_ARG_CAP) as u64;
    let slot_capacity = frame.arg(SYSCALL_ARG_PTR);
    if target_pid == 0 || slot_capacity == 0 {
        return Err(SyscallError::InvalidArgs);
    }
    // Stage 5B plan-first: snapshot task domain (rank 2) before capability
    // mutation (rank 4). When the global lock is removed, this read moves to
    // before the with_cpu() call via split-read on SharedKernel.
    let plan = ControlPlaneCnodePlan {
        requester_class: kernel
            .task_class(requester_tid)
            .ok_or(SyscallError::from(KernelError::TaskMissing))?,
        requester_pid: kernel.process_id(requester_tid).unwrap_or(requester_tid),
    };
    kernel
        .control_plane_set_process_cnode_slots_planned(&plan, target_pid, slot_capacity)
        .map_err(SyscallError::from)?;
    frame.set_ok(slot_capacity, target_pid as usize, 0);
    Ok(())
}
