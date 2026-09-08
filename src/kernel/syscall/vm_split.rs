// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-VM-ENTRY1 — the SPLIT adapter for NR 3 / NR 13 / NR 14, and the rank-local owners it
//! needed that did not yet exist off the broad lock.

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::capabilities::{CapId, CapObject};

// ═══════════════════════════════════════════════════════════════════════════════════════════
// U9-VM-ENTRY1 — the SPLIT adapter for NR 3 / NR 13 / NR 14.
//
// One method per domain acquisition, driving the SAME `vm_txn` policy the broad handlers drive.
// Three of its methods delegate to the very same rank-local bodies the broad adapter uses
// (`install_range_locked`, `settle_installed_locked`, `release_provisional_frame_cap_locked`), so
// two routes cannot disagree about installation, accounting or exclusivity — they are not two
// implementations of one contract, they are one implementation reached through two acquisitions.
// ═══════════════════════════════════════════════════════════════════════════════════════════

/// The split adapter. `tid` is the authoritative requester identity the trap seam resolved, never
/// an ambient current-task lookup: NR 3 may target another process's address space, and every
/// capability operation below must still name THIS caller's cspace.
pub(crate) struct SplitVmOwners<'a> {
    pub(crate) shared: &'a crate::runtime::SharedKernel,
    pub(crate) tid: u64,
}

impl crate::kernel::syscall::vm_txn::VmMapOwners for SplitVmOwners<'_> {
    fn caller_tid(&self) -> Option<u64> {
        Some(self.tid)
    }

    fn caller_asid(&self, tid: u64) -> Option<crate::kernel::vm::Asid> {
        self.shared.task_asid_option_split_read(tid)
    }

    fn resolve_address_space_cap(
        &self,
        tid: u64,
        cap: CapId,
    ) -> Result<crate::kernel::vm::Asid, KernelError> {
        let capability = self.shared.resolve_capability_for_task_split(tid, cap)?;
        match capability.object {
            CapObject::AddressSpace { asid } => Ok(crate::kernel::vm::Asid(asid)),
            _ => Err(KernelError::WrongObject),
        }
    }

    fn caller_cnode(&self, tid: u64) -> Option<crate::kernel::capabilities::CNodeId> {
        self.shared.task_cnode_split(tid)
    }

    fn is_page_mapped(
        &self,
        asid: crate::kernel::vm::Asid,
        virt: crate::kernel::vm::VirtAddr,
    ) -> Result<bool, KernelError> {
        self.shared.is_user_page_mapped_in_asid_split(asid, virt)
    }

    fn acquire_frame(
        &mut self,
        flags: crate::kernel::vm::PageFlags,
    ) -> Result<crate::kernel::syscall::vm_txn::ProvisionalFrame, KernelError> {
        self.shared.acquire_anonymous_frame_split(self.tid, flags)
    }

    fn install_range(
        &mut self,
        asid: crate::kernel::vm::Asid,
        base: usize,
        flags: crate::kernel::vm::PageFlags,
        frames: &[crate::kernel::syscall::vm_txn::ProvisionalFrame],
        out: &mut [crate::kernel::syscall::vm_txn::InstalledPage],
    ) -> Result<usize, (usize, KernelError)> {
        // ONE rank-5 acquisition covering install AND undo — the same body the broad adapter
        // runs, so the ownership proof is identical on both routes.
        self.shared.with_vm_user_spaces_split_mut(|spaces| {
            crate::kernel::syscall::vm::install_range_locked(spaces, asid, base, flags, frames, out)
        })
    }

    fn settle_installed(
        &mut self,
        asid: crate::kernel::vm::Asid,
        installed: &[crate::kernel::syscall::vm_txn::InstalledPage],
    ) {
        self.shared.with_memory_split_mut(|memory| {
            crate::kernel::syscall::vm::settle_installed_locked(memory, asid, installed);
        });
    }

    fn complete_shootdown(
        &mut self,
        asid: crate::kernel::vm::Asid,
        virt: crate::kernel::vm::VirtAddr,
    ) -> bool {
        // The existing U9-D3 coordinator: local invalidation, then a generation-matched remote
        // request each target acknowledges. NO domain lock is held across it.
        self.shared.complete_unmap_shootdown_split(asid, virt)
    }

    fn reclaim_replaced(&mut self, phys: crate::kernel::vm::PhysAddr) {
        self.shared.with_memory_split_mut(|memory| {
            KernelState::reclaim_memory_object_for_phys_locked(memory, phys)
        });
    }

    fn release_provisional_cap(
        &mut self,
        cnode: crate::kernel::capabilities::CNodeId,
        frame: crate::kernel::syscall::vm_txn::ProvisionalFrame,
    ) -> crate::kernel::syscall::vm_txn::ProvisionalReleaseOutcome {
        // ONE rank-4 acquisition, the same body the broad adapter runs: exact CapId generation,
        // exact object, no delegation link, no in-cspace child — all re-established HERE rather
        // than inherited from an earlier acquisition.
        self.shared.with_capability_state_split_mut(|capability| {
            crate::kernel::syscall::vm::release_provisional_frame_cap_locked(
                capability, cnode, frame,
            )
        })
    }

    fn account_released_cap(&mut self, frame: crate::kernel::syscall::vm_txn::ProvisionalFrame) {
        let object = CapObject::MemoryObject {
            id: frame.object_id,
        };
        self.shared.with_memory_split_mut(|memory| {
            KernelState::adjust_memory_object_cap_refcount_locked(memory, object, -1);
            KernelState::reclaim_memory_object_if_unreferenced_locked(memory, object);
        });
    }

    fn note(&mut self, event: crate::kernel::syscall::vm_txn::VmTxnEvent) {
        use crate::kernel::syscall::vm_txn::{VmRollbackReason, VmTxnEvent};
        match event {
            VmTxnEvent::Validated { asid, addr, len } => crate::yarm_log!(
                "VM_MAP_SPLIT_BEGIN asid={} addr=0x{:x} len={} broad_lock=0",
                asid.0,
                addr,
                len
            ),
            VmTxnEvent::FramesAcquired { count } => {
                crate::yarm_log!("VM_MAP_SPLIT_FRAMES_OK pages={}", count)
            }
            VmTxnEvent::Installed { count } => {
                crate::yarm_log!("VM_MAP_SPLIT_INSTALL_OK pages={}", count)
            }
            VmTxnEvent::RolledBack {
                reason,
                released,
                retained,
            } => {
                let reason = match reason {
                    VmRollbackReason::FrameAlloc => "frame_alloc",
                    VmRollbackReason::PageTableUpdate => "pt_update",
                };
                crate::yarm_log!(
                    "VM_MAP_SPLIT_ROLLBACK_OK reason={} released={} retained={}",
                    reason,
                    released,
                    retained
                );
            }
        }
    }
}

impl crate::kernel::syscall::vm_txn::VmBrkOwners for SplitVmOwners<'_> {
    fn caller_tid(&self) -> Option<u64> {
        Some(self.tid)
    }

    fn is_group_leader(&self, tid: u64) -> bool {
        // The delivered semantics: an absent task also reads as "not leader".
        self.shared.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.thread_group_id.0 == tid)
                .unwrap_or(false)
        })
    }

    fn caller_asid(&self, tid: u64) -> Option<crate::kernel::vm::Asid> {
        self.shared.task_asid_option_split_read(tid)
    }

    fn brk_bounds(&self, tid: u64) -> Option<(usize, usize)> {
        self.shared
            .with_memory_split_mut(|memory| KernelState::task_brk_bounds_locked(memory, tid))
    }

    fn task_exists(&self, tid: u64) -> bool {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| tcbs.iter().flatten().any(|tcb| tcb.tid.0 == tid))
    }

    fn set_brk_bounds(&mut self, tid: u64, base: usize, end: usize) -> Result<(), KernelError> {
        self.shared.with_memory_split_mut(|memory| {
            KernelState::set_task_brk_bounds_locked(memory, tid, base, end)
        })
    }

    fn unmap_brk_range(
        &mut self,
        asid: crate::kernel::vm::Asid,
        start: usize,
        end: usize,
    ) -> Result<usize, KernelError> {
        // U9-VM-ENTRY1: the topology restriction is GONE, and not by ignoring it.
        //
        // The delivered split shrink refused whenever more than one CPU was online, because the
        // only unmap cascade it could reach needed `request_live_asid_shootdown` and the ipc(3)
        // domain. `unmap_range_two_phase_split` is the owner that answered that: rank 5 removes
        // the PTE, the shootdown completes with NO lock held through the generation-matched
        // coordinator, and rank 6 reclaims only after the acknowledgement. It is correct at any
        // CPU count, so the shrink is too.
        //
        // A page whose shootdown was NOT acknowledged is deliberately left unreclaimed rather
        // than recycled under a possibly-stale remote translation; that is the owner's own
        // fail-closed rule and this route does not weaken it.
        let len = end.saturating_sub(start);
        let pages = len / crate::kernel::vm::PAGE_SIZE;
        let all_acked = self.shared.unmap_range_two_phase_split(asid, start, len);
        if !all_acked {
            crate::yarm_log!(
                "VM_BRK_SPLIT_SHOOTDOWN_INCOMPLETE asid={} start=0x{:x} end=0x{:x}",
                asid.0,
                start,
                end
            );
        }
        Ok(pages)
    }

    fn note_brk(&mut self, shape: crate::kernel::syscall::vm_txn::BrkShape, pages_unmapped: usize) {
        use crate::kernel::syscall::vm_txn::BrkShape;
        let shape = match shape {
            BrkShape::Query => "query",
            BrkShape::Growth => "growth",
            BrkShape::NoOp => "no_op",
            BrkShape::ShrinkUnmapping { .. } => "shrink_unmapping",
            BrkShape::ShrinkWithinPage => "shrink_within_page",
        };
        crate::yarm_log!(
            "VM_BRK_SPLIT_OK shape={} pages_unmapped={} broad_lock=0",
            shape,
            pages_unmapped
        );
    }
}

impl crate::runtime::SharedKernel {
    /// U9-VM-ENTRY1 — rank 5, one acquisition: the split twin of
    /// `KernelState::is_user_page_mapped_in_asid`, for the guard-page question.
    pub(crate) fn is_user_page_mapped_in_asid_split(
        &self,
        asid: crate::kernel::vm::Asid,
        virt: crate::kernel::vm::VirtAddr,
    ) -> Result<bool, KernelError> {
        use crate::kernel::vm::{PAGE_SIZE, VmError};
        if !virt.0.is_multiple_of(PAGE_SIZE as u64) {
            return Err(KernelError::Vm(VmError::Misaligned));
        }
        self.with_vm_user_spaces_split_mut(|spaces| match spaces.get_mut(asid) {
            Some(aspace) => Ok(aspace.resolve(virt).is_some()),
            None => Err(KernelError::Vm(VmError::InvalidAsid)),
        })
    }

    /// U9-VM-ENTRY1 — take one anonymous frame for `tid`, off the broad lock.
    ///
    /// The split twin of `alloc_anonymous_memory_object` followed by the delivered rights-checked
    /// phys resolve. Rank 6 (frame + object slot), then rank 4 (mint), then rank 4+6 (resolve) —
    /// sequential acquisitions, never nested.
    ///
    /// The mint goes through `mint_capability_with_memory_ref_split`, whose Model-A discipline
    /// exists for exactly this: the object's `cap_refcount` is bumped BEFORE any cnode slot can
    /// reference it, so no concurrent reclaim can free an object a freshly published slot already
    /// names. That helper has been `M2_SEAM_HELPER_ONLY` since Stage 186D-proper; this is its
    /// first live caller.
    ///
    /// Every failure after the first mutation compensates through the same narrow owners the
    /// transaction uses, so nothing is ever handed back partially executed.
    pub(crate) fn acquire_anonymous_frame_split(
        &self,
        tid: u64,
        flags: crate::kernel::vm::PageFlags,
    ) -> Result<crate::kernel::syscall::vm_txn::ProvisionalFrame, KernelError> {
        use crate::kernel::boot::MemoryObjectKind;
        use crate::kernel::capabilities::Capability;
        use crate::kernel::syscall::vm_txn::ProvisionalFrame;
        use crate::kernel::vm::{PAGE_SIZE, PhysAddr};

        let cnode = self.task_cnode_split(tid).ok_or(KernelError::TaskMissing)?;
        let max_objects = self.runtime_capacity_config_split_read().max_memory_objects;

        // Phase 1 (rank 6): the frame and its object slot, in ONE acquisition so a slot can never
        // exist without its backing or vice versa.
        let (object_id, phys) = self.with_memory_split_mut(|memory| {
            let phys = PhysAddr(
                crate::kernel::boot::kernel_mut(&mut memory.frame_allocator)
                    .alloc_contiguous(1)
                    .map_err(|err| match err {
                        crate::kernel::frame_allocator::FrameAllocError::OutOfMemory => {
                            KernelError::MemoryObjectFull
                        }
                        _ => KernelError::Vm(crate::kernel::vm::VmError::Full),
                    })?,
            );
            match KernelState::create_memory_object_slot_locked(
                memory,
                phys,
                PAGE_SIZE,
                MemoryObjectKind::Anonymous,
                max_objects,
            ) {
                Ok(id) => Ok((id, phys)),
                Err(e) => {
                    // The slot install refused, so the frame it would have described is ours to
                    // return — the same extent, to the same allocator.
                    crate::kernel::boot::kernel_mut(&mut memory.frame_allocator)
                        .free_contiguous(phys.0, 1);
                    Err(e)
                }
            }
        })?;

        let object = CapObject::MemoryObject { id: object_id };
        // Phase 2 (rank 6 then rank 4): the pre-bumped, atomically published mint.
        let cap = match self.mint_capability_with_memory_ref_split(
            cnode,
            Capability::new(
                object,
                KernelState::memory_object_rights_for_kind(MemoryObjectKind::Anonymous),
            ),
        ) {
            Ok(cap) => cap,
            Err(e) => {
                self.release_orphan_object_split(object_id);
                return Err(e);
            }
        };

        // Phase 3: the delivered RIGHTS check, resolved THROUGH the capability against these
        // exact flags — the same question the in-lock map asks before touching a page table.
        match self.resolve_memory_object_phys_for_task_split(tid, cap, flags) {
            Ok(resolved) => Ok(ProvisionalFrame {
                object_id,
                cap,
                phys: resolved,
            }),
            Err(e) => {
                let frame = ProvisionalFrame {
                    object_id,
                    cap,
                    phys,
                };
                if matches!(
                    self.with_capability_state_split_mut(|capability| {
                        crate::kernel::syscall::vm::release_provisional_frame_cap_locked(
                            capability, cnode, frame,
                        )
                    }),
                    crate::kernel::syscall::vm_txn::ProvisionalReleaseOutcome::Released
                ) {
                    self.with_memory_split_mut(|memory| {
                        KernelState::adjust_memory_object_cap_refcount_locked(memory, object, -1);
                        KernelState::reclaim_memory_object_if_unreferenced_locked(memory, object);
                    });
                }
                Err(e)
            }
        }
    }

    /// rank 6 — release an object that no capability ever came to reference.
    ///
    /// Reached only when the mint itself failed, so the object is unreachable by construction:
    /// nothing published a slot naming it. `release_memory_object_slot_locked` returns its backing
    /// according to that backing's own ownership rule, so an anonymous object returns its exact
    /// extent to the allocator.
    fn release_orphan_object_split(&self, object_id: u64) {
        self.with_memory_split_mut(|memory| {
            if let Some(slot) = memory
                .memory_objects
                .iter()
                .position(|entry| entry.is_some_and(|mem| mem.id == object_id))
            {
                KernelState::release_memory_object_slot_locked(memory, slot);
            }
        });
    }
}
