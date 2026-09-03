// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-FORK1 §2/§3 — THE copy-on-write address-space clone, as one rank-local transaction.
//!
//! # What this file is
//!
//! One clone policy, expressed against `&mut AddressSpaceManager` (VM, rank 5) and
//! `&mut MemorySubsystem` (rank 6) and nothing else, plus the exact inverse of everything it
//! does. `KernelState::clone_user_address_space_cow` becomes a one-line acquisition around
//! [`clone_address_space_cow_locked`], so the broad path and the split path execute the same
//! instructions rather than two implementations that can drift.
//!
//! # What changed, and why it had to
//!
//! The previous body was correct under the broad lock and only under the broad lock. It ran
//! `map_user_page_in_asid_raw` (vm→memory), then `with_user_spaces_mut` (vm), then `mark_cow_page`
//! (memory) twice — **per page** — so a clone of an N-page parent took and released the VM and
//! memory locks O(N) times. Under the broad lock nothing can observe the gaps; off it, another CPU
//! sees a parent that is half copy-on-write, and a fork can interleave with an unmap of the very
//! run it is about to write-protect. Holding both locks for the whole body is what makes the clone
//! one transaction instead of a sequence of them, and it is the only reason this file exists.
//!
//! # The parent's TLB obligation
//!
//! Write-protecting a parent run is a permission *downgrade*, and it owes exactly the invalidation
//! an unmap owes: a CPU still holding a writable translation writes straight through to the shared
//! frame and the child observes the parent's write. The local half is discharged inside the VM
//! lock — every architecture's `map_page` ends in its own `invalidate_page` — but the remote half
//! is not, and it cannot be, because a CPU may not be IPI'd while a domain lock is held.
//!
//! [`CowShootdownPlan`] is therefore returned, not performed. The caller releases both locks and
//! completes it through the existing generation-matched coordinator. What each architecture still
//! owes after the local invalidation is a property of its own invalidation instruction:
//!
//! | arch | write-protect invalidation | remote work owed |
//! |---|---|---|
//! | x86_64 | `invlpg` — this core only | yes: the 0xF1 coordinator |
//! | AArch64 | `tlbi vaae1is` — inner-shareable **broadcast** | none; hardware did it |
//! | RISC-V | `sfence.vma va, x0` — this hart only | yes, and it cannot be satisfied |
//!
//! [`remote_write_protect_work_is_owed`] states that, and it is why a RISC-V fork with a genuinely
//! remote translation holder must fail closed rather than proceed: a stale writable entry that
//! nothing can shoot down is a silently broken COW, which is strictly worse than a refused fork. In
//! every shipped workload the target set is empty — the forking task is the only thread of its
//! process — so the fast path is taken and nothing is skipped.
//!
//! # No frame is copied here
//!
//! The child maps the parent's EXACT physical backing. `note_mapping_inserted_locked`, inside the
//! mapper, takes each covering MemoryObject's `map_refcount` up by one per page; the child's
//! teardown takes the same count back down. A private copy is made later and only on demand, by
//! the existing COW fault owner, which this file does not touch.

use alloc::vec::Vec;

use super::defs::MemorySubsystem;
use super::{KernelError, KernelState};
use crate::kernel::vm::{
    AddressSpaceManager, Asid, Mapping, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr, VmError,
};

/// THE failure-site vocabulary. One constant per fallible step of
/// [`clone_address_space_cow_locked`], so a diagnostic names a step that provably exists rather
/// than a string that once matched one.
pub(crate) mod site {
    pub(crate) const PARENT_MISSING: &str = "parent_missing";
    pub(crate) const PREFLIGHT_CHILD: &str = "preflight_child";
    pub(crate) const CREATE_USER_SPACE: &str = "create_user_space";
    pub(crate) const MAP_CHILD: &str = "map_child";
    pub(crate) const WRITE_PROTECT_PARENT: &str = "write_protect_parent";
    pub(crate) const MARK_COW_PARENT: &str = "mark_cow_parent";
    pub(crate) const MARK_COW_CHILD: &str = "mark_cow_child";
    pub(crate) const MARK_COW_CHILD_INHERITED: &str = "mark_cow_child_inherited";
}

/// Every failure site the clone body can report, in body order. Exhaustive by construction: the
/// guard in `kernel/boot/tests.rs` counts the body's `CowCloneError::new(` arms against this list.
pub(crate) const ALL_COW_CLONE_SITES: [&str; 8] = [
    site::PARENT_MISSING,
    site::PREFLIGHT_CHILD,
    site::CREATE_USER_SPACE,
    site::MAP_CHILD,
    site::WRITE_PROTECT_PARENT,
    site::MARK_COW_PARENT,
    site::MARK_COW_CHILD,
    site::MARK_COW_CHILD_INHERITED,
];

/// A clone failure, with the exact step that produced it.
///
/// The site is a typed value, not a log line, because the body holds two domain locks and the
/// caller — which holds none — is the only place allowed to log. A caller therefore cannot name a
/// step the body does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CowCloneError {
    pub(crate) site: &'static str,
    /// The virtual address under construction, or `0` for the whole-clone sites.
    pub(crate) va: u64,
    pub(crate) err: KernelError,
}

impl CowCloneError {
    fn new(site: &'static str, va: u64, err: KernelError) -> Self {
        Self { site, va, err }
    }
}

/// One parent run this clone attempt write-protected, with the flags it had before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CowWpRun {
    pub(crate) virt: VirtAddr,
    /// The run head's flags BEFORE this attempt touched them. Rollback restores exactly these.
    pub(crate) original: PageFlags,
    pub(crate) pages: usize,
}

/// One run of pages, named by its head and length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CowRun {
    pub(crate) virt: VirtAddr,
    pub(crate) pages: usize,
}

/// The TLB work a clone owes once its locks are released.
///
/// Empty `runs` means no permission was downgraded, which is the whole obligation discharged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CowShootdownPlan {
    /// The address space whose permissions changed — always the PARENT.
    pub(crate) asid: Asid,
    /// Every downgraded run. Stored as runs, not pages, so a large mapping costs one entry.
    pub(crate) runs: Vec<CowRun>,
}

impl CowShootdownPlan {
    pub(crate) fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// Total pages whose permission changed.
    pub(crate) fn pages(&self) -> usize {
        self.runs.iter().map(|run| run.pages).sum()
    }
}

/// Whether this architecture still owes remote invalidation after the local one.
///
/// AArch64's `tlbi vaae1is` is inner-shareable: the broadcast already happened inside the VM lock,
/// so there is nothing left to coordinate, and claiming otherwise would be a lie in the unsafe
/// direction — it would let a caller believe a coordinator ran when none did. Every other
/// architecture invalidates locally only and therefore still owes the remote half.
pub(crate) const fn remote_write_protect_work_is_owed() -> bool {
    !cfg!(target_arch = "aarch64")
}

/// The identity-bearing provisional clone.
///
/// It carries everything needed to either commit the child or restore the parent, and it names the
/// exact incarnations it acted on, so a stale token cannot touch a recycled address space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CowCloneToken {
    pub(crate) parent_asid: Asid,
    pub(crate) child_asid: Asid,
    /// Monotonic, stamped by the caller. Never derived from either ASID, so a token for an earlier
    /// occupant of a recycled ASID pair cannot match a later clone.
    pub(crate) generation: u64,
    /// Parent runs write-protected by THIS attempt.
    pub(crate) wp: Vec<CowWpRun>,
    /// Runs mapped into the child by THIS attempt.
    pub(crate) child_runs: Vec<CowRun>,
    /// Pages that were already read-only AND already copy-on-write in the parent — inherited from
    /// an EARLIER fork — and whose mark this attempt propagated to the child. Recorded so the
    /// caller can report them: without the propagated mark the child's first write finds the page
    /// present and read-only but not COW, the handler declines, and the fault loops.
    pub(crate) inherited_cow: Vec<VirtAddr>,
    /// The parent's mapping count before the clone. A rollback proves it comes back to this.
    pub(crate) parent_mappings_before: usize,
    /// The TLB work the write-protection owes.
    pub(crate) shootdown: CowShootdownPlan,
}

impl CowCloneToken {
    /// The restoration shootdown a rollback owes: the same runs, now restored to writable.
    ///
    /// A downgrade needs invalidation so nobody keeps writing through a stale entry; the upgrade
    /// back needs it so nobody keeps faulting on one. It is the same run set by construction,
    /// which is what makes "every permission this attempt changed is accounted for in both
    /// directions" checkable rather than asserted.
    pub(crate) fn restoration_shootdown(&self) -> CowShootdownPlan {
        CowShootdownPlan {
            asid: self.parent_asid,
            runs: self
                .wp
                .iter()
                .map(|run| CowRun {
                    virt: run.virt,
                    pages: run.pages,
                })
                .collect(),
        }
    }
}

/// What a rollback did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CowCloneRollback {
    /// The child is gone and every parent run this attempt changed is back to its original flags.
    Restored { child_runs: usize, wp_runs: usize },
    /// Nothing to do: the child address space is already gone. Repeat-inert.
    AlreadyGone,
    /// The parent no longer exists — it was destroyed or its ASID recycled under us. Nothing is
    /// restored, because restoring would write another incarnation's permissions.
    StaleParent,
}

/// Take the parent's runs as they are, before anything is written.
///
/// Iterating the live table while mutating it is what an earlier revision did, and re-mapping a
/// page inside a multi-page run SPLIT that run, so the loop walked the split-off tails and grew
/// the parent to `MAX_MAPPINGS`. The snapshot is taken once and never re-read.
fn snapshot_parent_runs(
    vm: &AddressSpaceManager,
    parent_asid: Asid,
) -> Option<Vec<(VirtAddr, PhysAddr, PageFlags, usize)>> {
    let aspace = vm.get(parent_asid)?;
    let mut runs = Vec::new();
    let mut i = 0usize;
    while let Some((virt, mapping, pages)) = aspace.run_at(i) {
        runs.push((virt, mapping.phys, mapping.flags, pages));
        i += 1;
    }
    Some(runs)
}

/// THE copy-on-write clone. VM rank 5 and memory rank 6, held for the whole body, and nothing else.
///
/// # Order
///
/// 1. **Preflight**, before any mutation: the parent must exist and the child must fit. A refusal
///    here leaves the parent byte-identical because nothing has been written.
/// 2. Create the child address space.
/// 3. Per parent run, in snapshot order:
///    a. map every page of the run into the child at the SAME virtual address against the SAME
///       physical backing, with the write bit cleared — no frame is copied, and the mapper takes
///       the covering MemoryObject's `map_refcount` up by exactly one per page;
///    b. if the parent run is writable, write-protect its head IN PLACE (flags only — the entry
///       count is unchanged, so the parent table cannot grow) and record the original flags;
///    c. mark the run copy-on-write in both address spaces — or, for a run already read-only *and
///       already COW* from an earlier fork, mark it COW in the child too. That second case is not
///       an optimization: without the mark the child's first write finds the page present and
///       read-only but not COW, the fault handler declines, and the fault loops.
/// 4. Return the token, the parent's owed shootdown included.
///
/// Any failure inside step 3 rolls the whole attempt back through [`rollback_cow_clone_locked`] —
/// the same inverse the caller uses, so there is no second unwind policy — and returns the
/// ORIGINAL error, not the rollback's.
pub(crate) fn clone_address_space_cow_locked(
    vm: &mut AddressSpaceManager,
    memory: &mut MemorySubsystem,
    parent_asid: Asid,
    generation: u64,
) -> Result<CowCloneToken, CowCloneError> {
    let Some(snapshot) = snapshot_parent_runs(vm, parent_asid) else {
        return Err(CowCloneError::new(
            site::PARENT_MISSING,
            0,
            KernelError::Vm(VmError::InvalidAsid),
        ));
    };
    let parent_mappings_before = vm.get(parent_asid).map(|a| a.mappings()).unwrap_or(0);

    // ── 1. Preflight. The child needs at most one entry per parent run (merges only reduce).
    if snapshot.len() > crate::kernel::vm::MAX_MAPPINGS {
        return Err(CowCloneError::new(
            site::PREFLIGHT_CHILD,
            0,
            KernelError::Vm(VmError::Full),
        ));
    }

    // ── 2. The child address space.
    let child_asid = vm
        .create_user_space()
        .map_err(|e| CowCloneError::new(site::CREATE_USER_SPACE, 0, KernelError::Vm(e)))?;

    let mut token = CowCloneToken {
        parent_asid,
        child_asid,
        generation,
        wp: Vec::new(),
        child_runs: Vec::new(),
        inherited_cow: Vec::new(),
        parent_mappings_before,
        shootdown: CowShootdownPlan {
            asid: parent_asid,
            runs: Vec::new(),
        },
    };

    let page_sz = PAGE_SIZE as u64;
    for (virt, phys, flags, pages) in snapshot {
        let mut shared_flags = flags;
        shared_flags.write = false;

        // ── 3a. The child's view of this run: same VA, same backing, never writable.
        for p in 0..pages {
            let pv = VirtAddr(virt.0 + p as u64 * page_sz);
            let pp = PhysAddr(phys.0 + p as u64 * page_sz);
            if let Err(err) = super::vm_image_locked::map_user_page_in_asid_raw_locked(
                vm,
                memory,
                child_asid,
                pv,
                Mapping {
                    phys: pp,
                    flags: shared_flags,
                },
            ) {
                // Record the partial run BEFORE unwinding: the pages that did go in took
                // refcounts, and the unwind reads this list to give them back.
                if p > 0 {
                    token.child_runs.push(CowRun { virt, pages: p });
                }
                rollback_cow_clone_locked(vm, memory, &token);
                return Err(CowCloneError::new(site::MAP_CHILD, pv.0, err));
            }
            // The hosted build models user memory as an `(asid, phys) -> byte` map rather than a
            // real page table, so sharing a frame between two ASIDs has to be spelled out. The
            // freestanding build shares the actual frame and copies nothing — which is the whole
            // point of a COW fork, and is why this is `hosted-dev` only.
            #[cfg(feature = "hosted-dev")]
            for offset in 0..page_sz {
                if let Some(value) = memory
                    .user_memory
                    .get(&(parent_asid.0, pp.0 + offset))
                    .copied()
                {
                    memory
                        .user_memory
                        .insert((child_asid.0, pp.0 + offset), value);
                }
            }
        }
        token.child_runs.push(CowRun { virt, pages });

        if flags.write {
            // ── 3b. Downgrade the parent run head in place. No split, entry count fixed.
            let original = match vm
                .get_mut(parent_asid)
                .ok_or(VmError::InvalidAsid)
                .and_then(|a| a.write_protect_run_head_in_place(virt))
            {
                Ok(original) => original,
                Err(e) => {
                    rollback_cow_clone_locked(vm, memory, &token);
                    return Err(CowCloneError::new(
                        site::WRITE_PROTECT_PARENT,
                        virt.0,
                        KernelError::Vm(e),
                    ));
                }
            };
            token.wp.push(CowWpRun {
                virt,
                original,
                pages,
            });
            token.shootdown.runs.push(CowRun { virt, pages });

            // ── 3c. COW in both address spaces.
            for p in 0..pages {
                let pv = VirtAddr(virt.0 + p as u64 * page_sz);
                if let Err(err) = mark_cow_page_locked(memory, parent_asid, pv) {
                    rollback_cow_clone_locked(vm, memory, &token);
                    return Err(CowCloneError::new(site::MARK_COW_PARENT, pv.0, err));
                }
                if let Err(err) = mark_cow_page_locked(memory, child_asid, pv) {
                    rollback_cow_clone_locked(vm, memory, &token);
                    return Err(CowCloneError::new(site::MARK_COW_CHILD, pv.0, err));
                }
            }
        } else {
            // A run can be read-only in the parent yet still COW-shared: an earlier fork
            // write-protected it and the parent has not written since. The new child must inherit
            // the mark, or its first write finds the page present+RO but not COW, the handler
            // declines, and the fault loops. A genuinely read-only run (code, rodata) carries no
            // parent mark and is shared directly — a write there is a real protection fault.
            for p in 0..pages {
                let pv = VirtAddr(virt.0 + p as u64 * page_sz);
                if is_cow_page_locked(memory, parent_asid, pv) {
                    if let Err(err) = mark_cow_page_locked(memory, child_asid, pv) {
                        rollback_cow_clone_locked(vm, memory, &token);
                        return Err(CowCloneError::new(
                            site::MARK_COW_CHILD_INHERITED,
                            pv.0,
                            err,
                        ));
                    }
                    token.inherited_cow.push(pv);
                }
            }
        }
    }

    Ok(token)
}

/// THE exact inverse of [`clone_address_space_cow_locked`], in reverse order.
///
/// 1. The child address space, through the **never-resident** teardown — which unmaps every page
///    the clone installed, takes back the exact `map_refcount` increments those maps made, returns
///    the frames no MemoryObject describes, and consumes NO retired-ASID slot. That last point is
///    the whole reason it is this teardown and not the live one: a never-resident child owes no
///    shootdown, and the live teardown registers a retired ASID for every destroy, so a rollback
///    built on it burns one slot per failed fork until `destroy_and_collect_mappings` correctly
///    refuses with `VmError::Full` — turning an exact rollback into a leak. The clone's child is
///    never resident by construction: no TCB names its ASID until the publication, which happens
///    after every step that can still fail.
/// 2. Every parent run this attempt write-protected, restored to its ORIGINAL flags, in reverse
///    order, with the COW marks this attempt added cleared.
///
/// Refuses without mutation when the parent is gone ([`CowCloneRollback::StaleParent`]) and is
/// inert when the child is already gone ([`CowCloneRollback::AlreadyGone`]), so a repeated
/// rollback and a stale token both cost nothing. The parent's restoration shootdown is NOT
/// performed here — it is owed to the caller through [`CowCloneToken::restoration_shootdown`], for
/// the same reason the forward direction owes one.
pub(crate) fn rollback_cow_clone_locked(
    vm: &mut AddressSpaceManager,
    memory: &mut MemorySubsystem,
    token: &CowCloneToken,
) -> CowCloneRollback {
    if vm.get(token.parent_asid).is_none() {
        // Restoring flags into a recycled ASID would write another process's permissions.
        return CowCloneRollback::StaleParent;
    }
    if vm.get(token.child_asid).is_none() {
        // Already rolled back. Repeating the parent restore would clear COW marks that a LATER
        // fork legitimately owns, so a repeat is inert rather than "best effort".
        return CowCloneRollback::AlreadyGone;
    }

    // ── 1. The child, through the never-resident teardown.
    let _ = super::vm_image_locked::destroy_unresident_address_space_locked(
        vm,
        memory,
        token.child_asid,
    );

    // ── 2. The parent, in reverse order.
    let page_sz = PAGE_SIZE as u64;
    for run in token.wp.iter().rev() {
        if let Some(aspace) = vm.get_mut(token.parent_asid) {
            aspace.restore_run_head_flags_in_place(run.virt, run.original);
        }
        for p in 0..run.pages {
            KernelState::clear_cow_page_locked(
                memory,
                token.parent_asid,
                VirtAddr(run.virt.0 + p as u64 * page_sz),
            );
        }
    }

    CowCloneRollback::Restored {
        child_runs: token.child_runs.len(),
        wp_runs: token.wp.len(),
    }
}

/// `&mut MemorySubsystem` sibling of `KernelState::mark_cow_page`, for use inside a held rank-6
/// acquisition. Byte-identical policy, including the hosted capacity limit that failure-injection
/// tests drive the clone's unwind through.
pub(crate) fn mark_cow_page_locked(
    memory: &mut MemorySubsystem,
    asid: Asid,
    virt: VirtAddr,
) -> Result<(), KernelError> {
    #[cfg(test)]
    if let Some(limit) = memory.cow_page_capacity_limit {
        let total: usize = memory.cow_pages.values().map(|s| s.len()).sum();
        if total >= limit {
            return Err(KernelError::MemoryObjectFull);
        }
    }
    memory
        .cow_pages
        .entry(asid.0)
        .or_insert_with(alloc::collections::BTreeSet::new)
        .insert(virt.0);
    Ok(())
}

/// `&MemorySubsystem` sibling of `KernelState::is_cow_page`.
pub(crate) fn is_cow_page_locked(memory: &MemorySubsystem, asid: Asid, virt: VirtAddr) -> bool {
    memory
        .cow_pages
        .get(&asid.0)
        .is_some_and(|set| set.contains(&virt.0))
}
