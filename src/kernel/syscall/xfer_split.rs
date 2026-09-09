// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-XFER1 — the SPLIT adapter for NR 4 `TransferRelease`.
//!
//! One method per domain acquisition, driving the SAME [`crate::kernel::syscall::xfer_txn`] policy
//! the broad adapter drives. It holds no validation, no ordering and no refusal of its own: the
//! two routes are not two implementations of one contract, they are one implementation reached
//! through two acquisitions.
//!
//! Two owners are worth naming, because they are where this route differs from a naive
//! translation of the broad one:
//!
//! * **the unmap** goes to `unmap_range_two_phase_from_split`, which STATES its requester. The
//!   broad path's `execute_tlb_shootdown_wait_plan` reaches `request_live_asid_shootdown`, which
//!   impersonates the requester onto each target (`set_current_cpu`), drains the targets'
//!   mailboxes from the requester and calls `yield_current` — none of which is legal off the broad
//!   lock, and none of which is needed once the entering CPU is carried explicitly
//!   (U9-VM-ENTRY1-S §1).
//!
//! * **the revoke** goes to `revoke_user_held_capability_split`, NOT to
//!   `revoke_capability_no_vm_split`. That one serves the provisional-capability rollback sites,
//!   where the cap was minted moments earlier in the same syscall and never handed out, so its
//!   closure is provably empty and a 16-element stack array is sound. NR 4's root is a capability
//!   **userspace holds**: its delegated closure can cross processes to arbitrary depth, and its
//!   ABI admits every `CapObject` kind including `Reply`. Reusing the bounded composition would
//!   refuse authority the broad path serves.

use crate::kernel::capabilities::{CNodeId, CapId};
use crate::kernel::ipc::ThreadId;
use crate::kernel::syscall::xfer_txn::{XferReleaseOwners, XferTxnEvent};
use crate::kernel::vm::{Asid, VirtAddr};

/// The split adapter.
///
/// `tid` is the requester identity the trap seam resolved, and `cpu` is the CPU the trap actually
/// entered on — never `sched.current_cpu`, for the reason `SplitVmOwners` documents: that field is
/// one shared slot any CPU's broad acquisition may write, so excluding it from a shootdown target
/// set is not the same as excluding the requester.
pub(crate) struct SplitXferOwners<'a> {
    pub(crate) shared: &'a crate::runtime::SharedKernel,
    pub(crate) tid: u64,
    pub(crate) cpu: crate::kernel::scheduler::CpuId,
}

impl XferReleaseOwners for SplitXferOwners<'_> {
    fn caller_with_user_asid(&mut self) -> Option<(ThreadId, Asid)> {
        // rank 2. A kernel task has no user ASID, which is exactly the condition the broad
        // `current_task_has_user_asid` gate tested, so one read answers both halves.
        let asid = self.shared.task_asid_option_split_read(self.tid)?;
        Some((ThreadId(self.tid), asid))
    }

    fn registered_range(&mut self, owner: ThreadId, cap: CapId) -> Option<(VirtAddr, usize)> {
        self.shared.active_transfer_mapping_for_split(owner, cap)
    }

    fn caller_cnode(&mut self) -> Option<CNodeId> {
        self.shared.task_cnode_split(self.tid)
    }

    fn page_is_mapped(&mut self, asid: Asid, virt: VirtAddr) -> bool {
        self.shared
            .is_user_page_mapped_in_asid_split(asid, virt)
            .unwrap_or(false)
    }

    fn capability_release_is_reservable(&mut self, _cnode: CNodeId, cap: CapId) -> bool {
        // Phase V, from reads only: the root must resolve AND its whole closure must be
        // reservable on the heap. Building the reservation here is what makes an allocation
        // failure a PRE-MUTATION refusal — the U9-VM-ENTRY1 journal discipline. The reservation
        // is then rebuilt by the commit rather than carried across, because between the two the
        // only thing that can have changed is the closure itself, and the commit must act on
        // what is true when it runs.
        self.shared
            .plan_revoke_user_held_capability_split(self.tid, cap)
            .is_ok()
    }

    fn unmap_whole_range(&mut self, asid: Asid, base: usize, map_len: usize) -> bool {
        // rank 5 → TLB with NO lock held → rank 6, per page, with the requester stated.
        self.shared
            .unmap_range_two_phase_from_split(self.cpu, asid, base, map_len)
    }

    fn revoke_user_held_capability(&mut self, _cnode: CNodeId, cap: CapId) -> bool {
        self.shared
            .revoke_user_held_capability_split(self.tid, cap)
            .is_ok()
    }

    fn remove_registration(&mut self, owner: ThreadId, cap: CapId) -> bool {
        self.shared.remove_active_transfer_mapping_split(owner, cap)
    }

    fn account_release(&mut self, map_len: usize) {
        self.shared.note_shared_mem_released_split(map_len);
    }

    fn note(&mut self, event: XferTxnEvent) {
        crate::kernel::syscall::cap::note_xfer_event("split", event);
    }
}
