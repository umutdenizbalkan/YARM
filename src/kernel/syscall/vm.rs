// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! VM/MM syscall handlers (VmMap, VmAnonMap, VmBrk).
//!
//! Stage 145: mechanically split from the parent `syscall.rs` module with zero
//! behavior change. `syscall.rs` keeps minimal delegation shims so dispatch
//! routing remains explicit while VM mapping semantics stay owned by the
//! existing `KernelState` VM methods.
//!
//! U9-VM-ENTRY1: the three handlers no longer own their policy. Validation order, the
//! guard-page rule, error precedence, phase order, compensation and result encoding all live in
//! [`crate::kernel::syscall::vm_txn`]; what remains here is the BROAD ADAPTER — one method per
//! domain acquisition — plus the frame decoding each NR does. The split adapter in
//! `runtime.rs` implements the same trait, so the two routes cannot disagree about any of it.

use super::{
    SYSCALL_ARG_CAP, SYSCALL_ARG_INLINE_PAYLOAD0, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR, SyscallError,
};
use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::capabilities::CapId;
use crate::kernel::capabilities::CapObject;
use crate::kernel::syscall::vm_txn::{
    BrkShape, MapTarget, PageRecord, ProvisionalFrame, ProvisionalReleaseOutcome, VmBrkOwners,
    VmMapOwners, VmRollbackReason, VmTxnEvent, run_vm_brk_transaction, run_vm_map_transaction,
};
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::{Asid, Mapping, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};

/// The BROAD adapter. Every method is the delivered `KernelState` owner, called with the global
/// lock already held; the transaction module supplies the order.
pub(crate) struct BroadVmOwners<'a> {
    pub(crate) kernel: &'a mut KernelState,
}

impl VmMapOwners for BroadVmOwners<'_> {
    fn caller_tid(&self) -> Option<u64> {
        self.kernel.current_tid()
    }

    fn caller_asid(&self, tid: u64) -> Option<Asid> {
        self.kernel.task_asid(tid)
    }

    fn resolve_address_space_cap(&self, tid: u64, cap: CapId) -> Result<Asid, KernelError> {
        let capability = self.kernel.resolve_capability_for_task(tid, cap)?;
        match capability.object {
            CapObject::AddressSpace { asid } => Ok(Asid(asid)),
            _ => Err(KernelError::WrongObject),
        }
    }

    fn caller_cnode(&self, _tid: u64) -> Option<crate::kernel::capabilities::CNodeId> {
        // The delivered rollback allocates and revokes in the CURRENT task's cnode regardless of
        // which address space the mapping targets. Preserved verbatim.
        self.kernel.current_task_cnode()
    }

    fn is_page_mapped(&self, asid: Asid, virt: VirtAddr) -> Result<bool, KernelError> {
        self.kernel.is_user_page_mapped_in_asid(asid, virt)
    }

    fn acquire_object(&mut self, flags: PageFlags) -> Result<(u64, PhysAddr), KernelError> {
        // The delivered RIGHTS check, at the delivered point in the error precedence, evaluated
        // against the rights the mint WILL carry rather than through a published capability. Same
        // predicate as `resolve_memory_object_phys(cap, flags)`, no cap required.
        anonymous_rights_admit(flags)?;
        let (object_id, phys) = self.kernel.alloc_anonymous_object_without_cap()?;
        Ok((object_id, phys))
    }

    fn release_unminted_object(&mut self, object_id: u64) {
        self.kernel.release_unminted_anonymous_object(object_id);
    }

    fn mint_frame_cap(&mut self, object_id: u64, _phys: PhysAddr) -> Result<CapId, KernelError> {
        self.kernel.mint_anonymous_frame_cap(object_id)
    }

    fn install_and_account(
        &mut self,
        asid: Asid,
        flags: PageFlags,
        records: &mut [PageRecord],
    ) -> Result<(), (usize, KernelError)> {
        // ONE acquisition: VM rank 5, then memory rank 6 nested underneath — the legal direction.
        self.kernel.with_vm_then_memory_mut(|spaces, memory| {
            install_and_account_locked(spaces, memory, asid, flags, records)
        })
    }

    fn complete_shootdown(&mut self, asid: Asid, virt: VirtAddr) -> bool {
        // The delivered coordinator, called at the point the delivered replace path never called
        // it at all: `map_user_page_in_asid_raw_locked` reclaimed a displaced frame with no
        // shootdown between the PTE overwrite and the reclaim. Under the global lock no other CPU
        // could enter the kernel to observe it, but the frame could still be handed out while a
        // remote TLB held the old translation. Routing the displaced-frame reclaim through the
        // same required-ACK rule the rest of this transaction obeys closes that without widening
        // scope: the ONLY behaviour change is that a reclaim now waits for the acknowledgement it
        // always owed.
        self.kernel.shootdown_replaced_mapping(asid, virt).is_ok()
    }

    fn reclaim_replaced(&mut self, phys: PhysAddr) {
        self.kernel.with_memory_state_mut(|memory| {
            KernelState::reclaim_memory_object_for_phys_locked(memory, phys);
        });
    }

    fn release_provisional_cap(
        &mut self,
        cnode: crate::kernel::capabilities::CNodeId,
        frame: ProvisionalFrame,
    ) -> ProvisionalReleaseOutcome {
        self.kernel.with_capability_state_mut(|capability| {
            release_provisional_frame_cap_locked(capability, cnode, frame)
        })
    }

    fn account_released_cap(&mut self, frame: ProvisionalFrame) {
        let object = CapObject::MemoryObject {
            id: frame.object_id,
        };
        self.kernel.with_memory_state_mut(|memory| {
            KernelState::adjust_memory_object_cap_refcount_locked(memory, object, -1);
            KernelState::reclaim_memory_object_if_unreferenced_locked(memory, object);
        });
    }

    fn note(&mut self, event: VmTxnEvent) {
        // The delivered VM-COW diagnostic vocabulary, unchanged and still knob-gated.
        if !crate::kernel::boot::vm_cow_enabled() {
            return;
        }
        match event {
            VmTxnEvent::Validated { asid, addr, len } => {
                crate::yarm_log!(
                    "VM_MAP_PHASE_METADATA asid={} addr=0x{:x} len={}",
                    asid.0,
                    addr,
                    len
                );
            }
            VmTxnEvent::FramesAcquired { .. } => {}
            VmTxnEvent::Installed { count } => {
                crate::yarm_log!("VM_MAP_PHASE_FRAME_ALLOC pages={}", count);
                crate::yarm_log!("VM_MAP_PHASE_PT_UPDATE pages={}", count);
            }
            VmTxnEvent::RolledBack {
                reason,
                released,
                retained,
            } => {
                let reason = match reason {
                    VmRollbackReason::FrameAlloc => "frame_alloc",
                    VmRollbackReason::PageTableUpdate => "pt_update",
                    VmRollbackReason::CapabilityMint => "cap_mint",
                    VmRollbackReason::JournalReserve => "journal_reserve",
                };
                crate::yarm_log!(
                    "VM_MAP_ROLLBACK_OK reason={} released={} retained={}",
                    reason,
                    released,
                    retained
                );
            }
        }
    }
}

/// The delivered rights predicate for an anonymous frame, without a capability.
///
/// `resolve_memory_object_phys(cap, flags)` refuses `MissingRight` when the requested flags demand
/// a right the capability lacks. Every capability this transaction mints carries
/// `memory_object_rights_for_kind(Anonymous)`, so the same question can be asked of those rights
/// directly — same predicate, same error, same position in the precedence, and no published slot
/// needed to ask it.
pub(crate) fn anonymous_rights_admit(flags: PageFlags) -> Result<(), KernelError> {
    use crate::kernel::boot::MemoryObjectKind;
    use crate::kernel::capabilities::CapRights;
    let rights = KernelState::memory_object_rights_for_kind(MemoryObjectKind::Anonymous);
    if flags.read && !rights.contains(CapRights::READ) {
        return Err(KernelError::MissingRight);
    }
    if flags.write && !rights.contains(CapRights::WRITE) {
        return Err(KernelError::MissingRight);
    }
    Ok(())
}

/// ONE acquisition — VM rank 5, memory rank 6 nested underneath it — that owns the WHOLE
/// address-space phase of the request: every install, every map reference, every displaced-mapping
/// accounting, and, on any failure, the restoration of every page it touched, all before it
/// releases.
///
/// This is the function that makes compensation ownership-proving rather than byte-comparing.
/// `map_page` REPLACES, so a rollback that ran in a LATER acquisition could not distinguish the
/// page it installed from a competitor's replacement — not even by comparing the virtual and
/// physical address, since a competitor may legitimately map the same backing, and a freed frame
/// may be handed back out under the same physical address. There is no per-mapping identity token
/// to appeal to. What there is, is serialization: within this one acquisition no other transaction
/// can observe or alter this address space, so each page the undo removes is provably the page
/// this call installed, and each page it restores is provably the mapping this call displaced.
///
/// Two sub-passes, in this order and for a reason:
///
/// * **A — install.** Write every PTE, recording what each displaced and marking the record
///   `installed`. On the first failure, walk back over exactly the pages this pass wrote,
///   restoring each displaced mapping verbatim or removing a page that displaced nothing. No
///   accounting has run yet, so there is none to undo. The `installed` flags survive the return,
///   because those pages had live translations and therefore owe a shootdown.
/// * **B — account.** Only once the whole request is installed: take the map reference on every
///   inserted frame, and for every displaced page clear its COW mark and drop its map reference.
///   This pass cannot fail, which is what lets it hold the irreversible half — `clear_cow_page`
///   destroys a mark this transaction could not restore, so it must never run on a path that can
///   still put the displaced mappings back.
///
/// Taking the map reference here rather than in a later acquisition is what removes the interval
/// in which a page of this request would be live in the page table with `map_refcount == 0` — an
/// interval a competing mapping operation could observe, and whose displaced-frame reclaim would
/// then free backing this transaction still owns.
pub(crate) fn install_and_account_locked(
    spaces: &mut crate::kernel::vm::AddressSpaceManager,
    memory: &mut crate::kernel::boot::MemorySubsystem,
    asid: Asid,
    flags: PageFlags,
    records: &mut [PageRecord],
) -> Result<(), (usize, KernelError)> {
    use crate::kernel::vm::VmError;

    // ── Pass A: install the whole request.
    for i in 0..records.len() {
        let virt = records[i].virt;
        let mapping = Mapping {
            phys: records[i].phys,
            flags,
        };
        let Some(aspace) = spaces.get_mut(asid) else {
            undo_installed_locked(spaces, asid, &records[..i]);
            return Err((i, KernelError::Vm(VmError::InvalidAsid)));
        };
        match aspace.map_page(virt, mapping) {
            Ok(replaced) => {
                records[i].replaced = replaced;
                records[i].installed = true;
            }
            Err(e) => {
                undo_installed_locked(spaces, asid, &records[..i]);
                return Err((i, KernelError::Vm(e)));
            }
        }
    }

    // ── Pass B: account it. Cannot fail.
    for record in records.iter() {
        KernelState::note_mapping_inserted_locked(memory, record.phys);
        if let Some(old) = record.replaced {
            KernelState::clear_cow_page_locked(memory, asid, record.virt);
            KernelState::note_mapping_removed_locked(memory, old.phys);
        }
    }
    Ok(())
}

/// The undo half of pass A, inside the SAME acquisition as the install. A page is returned to
/// exactly the state the install found it in: the displaced mapping is re-installed verbatim, and
/// a page that displaced nothing is removed.
///
/// A failure here cannot be propagated — the acquisition is the transaction — so a re-install that
/// refuses leaves the page unmapped, which is strictly safer than leaving this transaction's frame
/// reachable. The pages stay marked `installed`, because a removed translation owes a shootdown
/// whether or not it was replaced by something else.
fn undo_installed_locked(
    spaces: &mut crate::kernel::vm::AddressSpaceManager,
    asid: Asid,
    installed: &[PageRecord],
) {
    let Some(aspace) = spaces.get_mut(asid) else {
        return;
    };
    for record in installed.iter().rev() {
        if !record.installed {
            continue;
        }
        match record.replaced {
            Some(old) => {
                let _ = aspace.map_page(record.virt, old);
            }
            None => {
                let _ = aspace.unmap_page(record.virt);
            }
        }
    }
}

/// rank 4, ONE acquisition: release a provisional capability if and only if it is STILL this
/// transaction's exclusive childless leaf.
///
/// Three checks, all inside this one acquisition, because exclusivity established in an earlier
/// acquisition says nothing about this one:
///
/// 1. the slot resolves for this exact `CapId` — which carries the slot GENERATION, so a slot
///    that was retired and re-minted fails here rather than being mistaken for ours;
/// 2. it holds this exact `MemoryObject { id }` — so a same-generation reuse for a different
///    object fails too;
/// 3. no delegation link names this cap, and `delete_if_leaf` finds no in-cspace child.
///
/// Anything else returns without touching a thing: the slot now belongs to whoever derived it,
/// and removing it would destroy another transaction's resource. That is a retained frame, not a
/// leaked one — it is still referenced, so its object stays alive and is reclaimed with its last
/// reference.
pub(crate) fn release_provisional_frame_cap_locked(
    capability: &mut crate::kernel::boot::CapabilitySubsystem,
    cnode: crate::kernel::capabilities::CNodeId,
    frame: ProvisionalFrame,
) -> ProvisionalReleaseOutcome {
    use crate::kernel::boot::kernel_mut;
    // (3a) A delegation link naming this cap makes it non-exclusive. Conservative on purpose: the
    // numeric match alone is enough to decline, and declining never removes anything.
    if kernel_ref_links(capability)
        .iter()
        .flatten()
        .any(|link| link.source_cap == frame.cap)
    {
        return ProvisionalReleaseOutcome::Derived;
    }
    let Some(space) = capability
        .cnode_spaces
        .iter_mut()
        .flatten()
        .find(|space| space.id == cnode)
    else {
        return ProvisionalReleaseOutcome::NotOurs;
    };
    let cspace = kernel_mut(&mut space.cspace);
    // (1)+(2) exact slot generation, exact object.
    match cspace.get(frame.cap) {
        Some(capability) => {
            let expected = CapObject::MemoryObject {
                id: frame.object_id,
            };
            if capability.object != expected {
                return ProvisionalReleaseOutcome::NotOurs;
            }
        }
        None => return ProvisionalReleaseOutcome::NotOurs,
    }
    // (3b) the in-cspace derivation check, and the removal, in one step.
    match cspace.delete_if_leaf(frame.cap) {
        Ok(true) => ProvisionalReleaseOutcome::Released,
        Ok(false) => ProvisionalReleaseOutcome::Derived,
        Err(_) => ProvisionalReleaseOutcome::NotOurs,
    }
}

fn kernel_ref_links(
    capability: &crate::kernel::boot::CapabilitySubsystem,
) -> &[Option<crate::kernel::boot::DelegatedCapabilityLink>] {
    crate::kernel::boot::kernel_ref(&capability.delegated_capability_links).as_slice()
}

impl VmBrkOwners for BroadVmOwners<'_> {
    fn caller_tid(&self) -> Option<u64> {
        self.kernel.current_tid()
    }
    fn is_group_leader(&self, tid: u64) -> bool {
        self.kernel.is_thread_group_leader(tid)
    }
    fn caller_asid(&self, tid: u64) -> Option<Asid> {
        self.kernel.task_asid(tid)
    }
    fn brk_bounds(&self, tid: u64) -> Option<(usize, usize)> {
        self.kernel.task_brk_bounds(tid)
    }
    fn task_exists(&self, tid: u64) -> bool {
        self.kernel
            .with_tcbs(|tcbs| tcbs.iter().flatten().any(|tcb| tcb.tid.0 == tid))
    }
    fn set_brk_bounds(&mut self, tid: u64, base: usize, end: usize) -> Result<(), KernelError> {
        self.kernel.set_task_brk_bounds(tid, base, end)
    }
    fn unmap_brk_range(
        &mut self,
        asid: Asid,
        start: usize,
        end: usize,
    ) -> Result<usize, KernelError> {
        // The delivered two-phase shrink owner, unchanged: PTE remove → shootdown wait → reclaim.
        // The delivered two-phase shrink owner, unchanged: PTE remove -> shootdown wait ->
        // reclaim. Its second lane is the shootdown count, which the transaction does not need.
        self.kernel
            .vm_brk_shrink_two_phase(asid, start, end)
            .map(|(pages_unmapped, _shootdowns)| pages_unmapped)
    }
    fn note_brk(&mut self, shape: BrkShape, pages_unmapped: usize) {
        let _ = (shape, pages_unmapped);
    }
}

pub(super) fn handle_vm_map(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let aspace_map_cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let addr = frame.arg(SYSCALL_ARG_PTR);
    let len = frame.arg(SYSCALL_ARG_LEN);
    let prot = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0);
    let mut owners = BroadVmOwners { kernel };
    let (base, map_len) = run_vm_map_transaction(
        &mut owners,
        MapTarget::Capability(aspace_map_cap),
        addr,
        len,
        prot,
    )?;
    frame.set_ok(base, map_len, 0);
    Ok(())
}

pub(super) fn handle_vm_anon_map(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let addr = frame.arg(SYSCALL_ARG_PTR);
    let len = frame.arg(SYSCALL_ARG_LEN);
    let prot = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0);
    let mut owners = BroadVmOwners { kernel };
    let (base, map_len) =
        run_vm_map_transaction(&mut owners, MapTarget::CallerAddressSpace, addr, len, prot)?;
    frame.set_ok(base, map_len, 0);
    Ok(())
}

pub(super) fn handle_vm_brk(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let requested = frame.arg(SYSCALL_ARG_CAP);
    let mut owners = BroadVmOwners { kernel };
    let result = run_vm_brk_transaction(&mut owners, requested)?;
    frame.set_ok(result, 0, 0);
    Ok(())
}
