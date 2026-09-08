// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-VM-ENTRY1 — THE anonymous-mapping transaction, and the brk transaction.
//!
//! `NR 3 VmMap` and `NR 13 VmAnonMap` are the same operation with two different authorities:
//! NR 3 maps into the address space its capability names, NR 13 into the caller's own. NR 14
//! `VmBrk` is a different operation that shares this module's compensation discipline. All three
//! are stated once here so the broad and split adapters cannot come to disagree about validation
//! order, guard-page policy, error precedence, result encoding or rollback.
//!
//! # Why the phases are shaped the way they are
//!
//! The broad handlers could interleave "allocate one frame, map one frame" because the global
//! lock made the whole loop atomic. Off that lock it is not, and two obligations follow that the
//! in-lock shape cannot meet:
//!
//! * **Mapping compensation must prove OWNERSHIP, not matching bytes.** `AddressSpace::map_page`
//!   REPLACES, and comparing a recorded `(va, phys)` on the way out does not exclude a concurrent
//!   replacement that happens to reuse the same physical backing, nor an ABA reuse of the frame.
//!   So installation and rollback are performed inside ONE `user_spaces` acquisition: within it no
//!   other transaction can observe or alter this address space, so every page this transaction
//!   removes on the failure path is provably the page it installed, by serialization rather than
//!   by comparison. That is also what lets the failure path RESTORE the exact mapping each page
//!   displaced, so a partial failure removes no pre-existing mapping at all.
//!
//! * **A provisional capability is not private.** Every thread of a process shares one CNode
//!   (`task_cnode` resolves through `thread_group_id`), so a sibling running on another CPU can in
//!   principle reach a slot this transaction has minted but not yet returned. "Freshly minted" is
//!   therefore not a proof of exclusivity, and a childlessness snapshot taken in one acquisition
//!   says nothing about the next. The release below re-establishes identity INSIDE the one
//!   capability acquisition that removes the slot: the exact `CapId` (which carries the slot
//!   generation), the exact `MemoryObject` id, no delegation link naming it, and
//!   `delete_if_leaf`'s own in-cspace child scan. A cap that fails any of those is NOT this
//!   transaction's to remove and is left exactly as it is.
//!
//! Hence: acquire every resource first, install the whole range under one VM acquisition, and do
//! all rank-6 accounting afterwards. The failure path at each phase undoes exactly that phase's
//! own work through the same owners.
//!
//! # What this module deliberately does NOT do
//!
//! It never reaches general capability revocation (`revoke_capability_in_cnode`'s
//! delegated-descendant closure, active-transfer-mapping unmap, notification destroy). The
//! provisional caps it releases are ones it minted and still exclusively owns; anything else is
//! left alone rather than torn down. And it introduces no new user-visible refusal: every input
//! the broad handlers accept is accepted here, with the same error for every input they reject.

use crate::kernel::boot::KernelError;
use crate::kernel::capabilities::{CNodeId, CapId};
use crate::kernel::syscall::SyscallError;
use crate::kernel::vm::{Asid, Mapping, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};

/// The maximum number of pages one `VmMap`/`VmAnonMap` call may install.
///
/// The broad handlers are bounded only by the frame allocator, but this transaction records one
/// entry per page so that installation and rollback can share a single VM acquisition — and that
/// record has to live somewhere bounded, because a `no_std` kernel transaction must not depend on
/// a heap allocation succeeding in order to be able to roll back.
///
/// A request above the bound is NOT a new refusal: it is serviced page-run by page-run, each run
/// a complete transaction of its own, so userspace observes exactly the mapping it asked for. The
/// value is the same order as the largest run any live profile issues.
pub(crate) const VM_MAP_MAX_PAGES_PER_RUN: usize = 64;

/// One frame this transaction acquired and still owns: the object it created, the capability slot
/// it minted for the caller, and the physical extent both name.
///
/// All three travel together because the release path needs all three: the `CapId` to identify the
/// slot, the object `id` to prove the slot still holds THIS object, and the `phys` to account the
/// frame once the slot is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProvisionalFrame {
    pub(crate) object_id: u64,
    pub(crate) cap: CapId,
    pub(crate) phys: PhysAddr,
}

/// What one page's installation displaced, recorded inside the VM acquisition that displaced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InstalledPage {
    pub(crate) virt: VirtAddr,
    pub(crate) inserted: PhysAddr,
    /// The mapping that was already there, if any. On the failure path this is restored verbatim;
    /// on the success path its frame is accounted and reclaimed after its shootdown.
    pub(crate) replaced: Option<Mapping>,
}

/// Why a provisional capability was not released.
///
/// Recorded rather than ignored: each variant means the slot is no longer exclusively this
/// transaction's, and leaving it is the correct outcome — removing it would destroy a resource
/// another transaction now owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProvisionalReleaseOutcome {
    /// The slot still held this exact object as a childless, undelegated leaf, and was removed.
    Released,
    /// The slot no longer resolves, or holds a different object: already retired by someone else,
    /// whose retirement did the accounting.
    NotOurs,
    /// The slot still holds this object but has an in-cspace child or a delegation link naming it.
    /// Someone derived from it while this transaction was in flight.
    Derived,
}

/// The validated `(addr, len, prot)` triple, and the flags it decodes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValidatedMapArgs {
    pub(crate) addr: usize,
    pub(crate) map_len: usize,
    pub(crate) end: usize,
    pub(crate) flags: PageFlags,
}

/// Which authority names the target address space. This is the ONLY difference between NR 3 and
/// NR 13, and stating it as a type is what stops an adapter from substituting the ambient current
/// task for a capability-named target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MapTarget {
    /// NR 3: the address space named by this capability, resolved in the CALLER's cspace.
    Capability(CapId),
    /// NR 13: the caller's own address space.
    CallerAddressSpace,
}

/// Every domain operation the mapping transaction performs. One method, one acquisition of one
/// domain. No method contains phase order, validation sequencing or rollback policy.
pub(crate) trait VmMapOwners {
    /// The calling thread. `None` means no current task.
    fn caller_tid(&self) -> Option<u64>;

    /// rank 2 — the caller's own address space, or `None` when it has none.
    fn caller_asid(&self, tid: u64) -> Option<Asid>;

    /// rank 4 — resolve `cap` in the CALLER's cspace and return the address space it names.
    /// `InvalidCapability` when the slot does not resolve; `WrongObject` when it is not an
    /// `AddressSpace`.
    fn resolve_address_space_cap(&self, tid: u64, cap: CapId) -> Result<Asid, KernelError>;

    /// rank 4 — the caller's cspace, which is where every provisional cap is minted. Note this is
    /// the PROCESS cnode, shared with every sibling thread.
    fn caller_cnode(&self, tid: u64) -> Option<CNodeId>;

    /// rank 5 — is `virt` mapped in `asid`? The guard-page question, asked of the TARGET address
    /// space rather than the ambient one.
    fn is_page_mapped(&self, asid: Asid, virt: VirtAddr) -> Result<bool, KernelError>;

    /// rank 6 then rank 4 — take one frame, create its memory object, mint the caller's capability
    /// for it, and resolve the physical extent THROUGH that capability against `flags`.
    ///
    /// Sequential acquisitions, never nested. On a mint failure the object and its frame are
    /// released before returning, so no orphan is left. Resolving the phys through the capability
    /// rather than remembering the allocation is what preserves the delivered RIGHTS check: the
    /// same `resolve_memory_object_phys(cap, flags)` the in-lock map performs, at the same point
    /// relative to the mint, with the same error.
    fn acquire_frame(&mut self, flags: PageFlags) -> Result<ProvisionalFrame, KernelError>;

    /// ONE rank-5 acquisition covering the whole range: install `frames[i]` at
    /// `base + i * PAGE_SIZE`, recording what each displaced. On the first failure, restore every
    /// page already installed IN THIS SAME ACQUISITION and return the error together with the
    /// index that failed.
    ///
    /// Installation and rollback share one acquisition precisely so the rollback provably removes
    /// only pages this transaction installed.
    fn install_range(
        &mut self,
        asid: Asid,
        base: usize,
        flags: PageFlags,
        frames: &[ProvisionalFrame],
        out: &mut [InstalledPage],
    ) -> Result<usize, (usize, KernelError)>;

    /// rank 6 — account the pages that stayed installed: `map_refcount++` for each inserted frame,
    /// `map_refcount--` plus COW clear for each displaced one.
    fn settle_installed(&mut self, asid: Asid, installed: &[InstalledPage]);

    /// NO LOCK — complete the required TLB shootdown for `virt` in `asid`. Returns `false` when a
    /// remote acknowledgement was not obtained, in which case the caller must skip the reclaim.
    fn complete_shootdown(&mut self, asid: Asid, virt: VirtAddr) -> bool;

    /// rank 6 — reclaim a displaced frame, only ever after its shootdown completed.
    fn reclaim_replaced(&mut self, phys: PhysAddr);

    /// ONE rank-4 acquisition — release a provisional capability if and only if it is still, at
    /// this instant, this transaction's exclusive childless leaf naming `frame.object_id`.
    fn release_provisional_cap(
        &mut self,
        cnode: CNodeId,
        frame: ProvisionalFrame,
    ) -> ProvisionalReleaseOutcome;

    /// rank 6 — the accounting a released capability owes: `cap_refcount--` and a guarded reclaim.
    /// Called ONLY after [`Self::release_provisional_cap`] returned `Released`.
    fn account_released_cap(&mut self, frame: ProvisionalFrame);

    /// Diagnostic only. The broad adapter emits the delivered `VM_MAP_*` markers; the split
    /// adapter emits its own. Neither decides anything.
    fn note(&mut self, _event: VmTxnEvent) {}
}

/// The observable events of one mapping transaction, so each adapter can keep its own delivered
/// vocabulary without either of them owning policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VmTxnEvent {
    Validated {
        asid: Asid,
        addr: usize,
        len: usize,
    },
    FramesAcquired {
        count: usize,
    },
    Installed {
        count: usize,
    },
    RolledBack {
        reason: VmRollbackReason,
        released: usize,
        retained: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VmRollbackReason {
    FrameAlloc,
    PageTableUpdate,
}

/// Validates the `(addr, len, prot)` triple shared by NR 3 and NR 13, in the delivered order and
/// with the delivered errors.
pub(crate) fn validate_map_args(
    addr: usize,
    len: usize,
    prot: usize,
) -> Result<ValidatedMapArgs, SyscallError> {
    use crate::kernel::syscall::{
        SYSCALL_VM_MAP_PROT_EXEC, SYSCALL_VM_MAP_PROT_READ, SYSCALL_VM_MAP_PROT_WRITE,
        round_up_page,
    };
    if len == 0 || !addr.is_multiple_of(PAGE_SIZE) {
        return Err(SyscallError::InvalidArgs);
    }
    let map_len = round_up_page(len)?;
    let end = addr.checked_add(map_len).ok_or(SyscallError::InvalidArgs)?;
    let unknown =
        prot & !(SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE | SYSCALL_VM_MAP_PROT_EXEC);
    if unknown != 0 {
        return Err(SyscallError::InvalidArgs);
    }
    let flags = PageFlags {
        read: (prot & SYSCALL_VM_MAP_PROT_READ) != 0,
        write: (prot & SYSCALL_VM_MAP_PROT_WRITE) != 0,
        execute: (prot & SYSCALL_VM_MAP_PROT_EXEC) != 0,
        user: true,
        cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
    };
    Ok(ValidatedMapArgs {
        addr,
        map_len,
        end,
        flags,
    })
}

/// Resolve the TARGET address space for this NR's authority.
///
/// NR 3 resolves the capability the caller named, in the CALLER's cspace, and requires it to be an
/// `AddressSpace`. NR 13 uses the caller's own. Neither substitutes the other: a capability that
/// names another process's address space maps THERE, while the provisional frame capabilities are
/// still minted in the caller's own cnode — two owners, one transaction, exactly as delivered.
pub(crate) fn resolve_map_target<O: VmMapOwners>(
    owners: &O,
    tid: u64,
    target: MapTarget,
) -> Result<Asid, SyscallError> {
    match target {
        MapTarget::Capability(cap) => owners
            .resolve_address_space_cap(tid, cap)
            .map_err(SyscallError::from),
        MapTarget::CallerAddressSpace => owners
            .caller_asid(tid)
            .ok_or(SyscallError::from(KernelError::UserMemoryFault)),
    }
}

/// The stack-guard rule, asked of the TARGET address space.
///
/// Refuse a write-but-not-execute mapping whose immediately preceding page is already mapped. The
/// condition and the error are the delivered ones.
pub(crate) fn guard_page_refuses<O: VmMapOwners>(
    owners: &O,
    asid: Asid,
    args: &ValidatedMapArgs,
) -> Result<bool, SyscallError> {
    if !(args.flags.write && !args.flags.execute) {
        return Ok(false);
    }
    let Some(guard_page) = args.addr.checked_sub(PAGE_SIZE) else {
        return Ok(false);
    };
    owners
        .is_page_mapped(asid, VirtAddr(guard_page as u64))
        .map_err(SyscallError::from)
}

/// Release every provisional frame in `frames`, and report how many were actually this
/// transaction's to release.
///
/// Returns `(released, retained)`. A retained frame is one a sibling took ownership of while this
/// transaction was in flight; its capability and its object stay exactly as they are, which is the
/// only correct outcome — the frame is still referenced, so it is not leaked, and removing it
/// would destroy another transaction's resource.
fn release_frames<O: VmMapOwners>(
    owners: &mut O,
    cnode: Option<CNodeId>,
    frames: &[ProvisionalFrame],
) -> (usize, usize) {
    let Some(cnode) = cnode else {
        // No cspace to release into. Nothing was minted there either, so there is nothing to undo.
        return (0, frames.len());
    };
    let mut released = 0usize;
    let mut retained = 0usize;
    for frame in frames {
        match owners.release_provisional_cap(cnode, *frame) {
            ProvisionalReleaseOutcome::Released => {
                owners.account_released_cap(*frame);
                released += 1;
            }
            ProvisionalReleaseOutcome::NotOurs => {
                // Someone else's retirement already did this frame's accounting.
                retained += 1;
            }
            ProvisionalReleaseOutcome::Derived => retained += 1,
        }
    }
    (released, retained)
}

/// THE anonymous-mapping transaction: NR 3 and NR 13, one policy.
///
/// Phase order, and what each failure settles:
///
/// | phase | on failure |
/// |---|---|
/// | validate, resolve target, guard page | nothing acquired, nothing installed |
/// | R: acquire every frame | release exactly the frames acquired so far |
/// | I: install the whole range under ONE VM acquisition | that acquisition restores every page it displaced, then release every frame |
/// | S: account, shoot down, reclaim displaced frames | — the transaction has committed |
///
/// Returns the `(addr, map_len)` the caller's result lanes carry.
pub(crate) fn run_vm_map_transaction<O: VmMapOwners>(
    owners: &mut O,
    target: MapTarget,
    addr: usize,
    len: usize,
    prot: usize,
) -> Result<(usize, usize), SyscallError> {
    let args = validate_map_args(addr, len, prot)?;
    let tid = owners
        .caller_tid()
        .ok_or(SyscallError::from(KernelError::TaskMissing))?;
    let asid = resolve_map_target(owners, tid, target)?;
    if guard_page_refuses(owners, asid, &args)? {
        return Err(SyscallError::InvalidArgs);
    }
    owners.note(VmTxnEvent::Validated {
        asid,
        addr: args.addr,
        len: args.map_len,
    });

    let cnode = owners.caller_cnode(tid);
    let total_pages = args.map_len / PAGE_SIZE;
    let mut base = args.addr;
    let mut remaining = total_pages;

    // A request longer than one run is serviced as consecutive complete transactions. Each run
    // commits on its own, so a failure in a later run leaves the earlier runs installed — which is
    // exactly the delivered behaviour, whose rollback also only covers `[addr, mapped_end)`.
    while remaining > 0 {
        let run_pages = remaining.min(VM_MAP_MAX_PAGES_PER_RUN);
        run_one_map_run(owners, asid, cnode, base, run_pages, args.flags)?;
        base += run_pages * PAGE_SIZE;
        remaining -= run_pages;
    }
    Ok((args.addr, args.map_len))
}

fn run_one_map_run<O: VmMapOwners>(
    owners: &mut O,
    asid: Asid,
    cnode: Option<CNodeId>,
    base: usize,
    pages: usize,
    flags: PageFlags,
) -> Result<(), SyscallError> {
    debug_assert!(pages <= VM_MAP_MAX_PAGES_PER_RUN);
    let empty = ProvisionalFrame {
        object_id: 0,
        cap: CapId(0),
        phys: PhysAddr(0),
    };
    let mut frames = [empty; VM_MAP_MAX_PAGES_PER_RUN];

    // ── Phase R: every frame first. Nothing is installed yet, so a failure here has no VM effect
    // to undo — only the frames already taken.
    for i in 0..pages {
        match owners.acquire_frame(flags) {
            Ok(frame) => frames[i] = frame,
            Err(e) => {
                let (released, retained) = release_frames(owners, cnode, &frames[..i]);
                owners.note(VmTxnEvent::RolledBack {
                    reason: VmRollbackReason::FrameAlloc,
                    released,
                    retained,
                });
                return Err(SyscallError::from(e));
            }
        }
    }
    owners.note(VmTxnEvent::FramesAcquired { count: pages });

    // ── Phase I: ONE VM acquisition installs the whole run and, on failure, restores every page
    // it displaced before releasing that acquisition. Ownership of what the failure path removes
    // is established by that serialization, not by comparing a recorded frame number.
    let mut installed = [InstalledPage {
        virt: VirtAddr(0),
        inserted: PhysAddr(0),
        replaced: None,
    }; VM_MAP_MAX_PAGES_PER_RUN];
    let count =
        match owners.install_range(asid, base, flags, &frames[..pages], &mut installed[..pages]) {
            Ok(count) => count,
            Err((_failed_index, e)) => {
                // The acquisition already restored the address space. The frames are still ours.
                let (released, retained) = release_frames(owners, cnode, &frames[..pages]);
                owners.note(VmTxnEvent::RolledBack {
                    reason: VmRollbackReason::PageTableUpdate,
                    released,
                    retained,
                });
                return Err(SyscallError::from(e));
            }
        };
    debug_assert_eq!(count, pages);
    owners.note(VmTxnEvent::Installed { count });

    // ── Phase S: the run has committed. Account it, then retire whatever it displaced —
    // shootdown BEFORE reclaim, with no domain lock held across the wait.
    owners.settle_installed(asid, &installed[..count]);
    for page in &installed[..count] {
        if let Some(old) = page.replaced {
            if owners.complete_shootdown(asid, page.virt) {
                owners.reclaim_replaced(old.phys);
            }
        }
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════════════════
// NR 14 — VmBrk, complete.
//
// The delivered handler has five shapes and the delivered split route serviced exactly one of
// them, only at one CPU online. The policy below is the delivered one, whole: the group-leader
// rule, the query, the bounds, both shrink shapes, growth and the no-op, in the delivered order
// with the delivered errors. What changes is that its unmap reaches the same
// unmap → required ACK → reclaim owner the mapping transaction uses, so the topology restriction
// is gone rather than reproduced.
// ═══════════════════════════════════════════════════════════════════════════════════════════

/// What one `VmBrk` call resolved to. Naming the shapes is what makes "every shape is serviced"
/// checkable instead of asserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrkShape {
    /// `requested == 0` — report the current end, change nothing.
    Query,
    /// `requested > current_end` — bounds move up; pages stay lazy.
    Growth,
    /// `requested == current_end` — bounds rewritten to the same value.
    NoOp,
    /// `requested < current_end` and the page-rounded window is non-empty: pages are unmapped.
    ShrinkUnmapping {
        unmap_start: usize,
        unmap_end: usize,
    },
    /// `requested < current_end` but both ends round to the same page: bounds only.
    ShrinkWithinPage,
}

/// Every domain operation the brk transaction performs.
pub(crate) trait VmBrkOwners {
    fn caller_tid(&self) -> Option<u64>;
    /// rank 2 — is `tid` its own thread-group leader? An absent task also reads as "not leader",
    /// which is the delivered semantics of `is_thread_group_leader`.
    fn is_group_leader(&self, tid: u64) -> bool;
    /// rank 2 — the caller's address space, needed only by the unmapping shape.
    fn caller_asid(&self, tid: u64) -> Option<Asid>;
    /// rank 6 — the current `[base, end)` window, or `None` when the task has none.
    fn brk_bounds(&self, tid: u64) -> Option<(usize, usize)>;
    /// rank 2 — does the task still exist? The task-existence half of `set_task_brk_bounds`.
    fn task_exists(&self, tid: u64) -> bool;
    /// rank 6 — write the window.
    fn set_brk_bounds(&mut self, tid: u64, base: usize, end: usize) -> Result<(), KernelError>;
    /// rank 5 → no lock → rank 6, per page: remove the PTE, complete the required shootdown with
    /// NO lock held, then reclaim. Returns the number of pages actually unmapped.
    fn unmap_brk_range(
        &mut self,
        asid: Asid,
        start: usize,
        end: usize,
    ) -> Result<usize, KernelError>;
    fn note_brk(&mut self, _shape: BrkShape, _pages_unmapped: usize) {}
}

/// Classify one `VmBrk` request. Pure: it acquires nothing and mutates nothing, so every refusal
/// it produces is pre-mutation by construction.
pub(crate) fn classify_brk(
    requested: usize,
    base: usize,
    current_end: usize,
) -> Result<BrkShape, SyscallError> {
    use crate::kernel::syscall::round_up_page;
    if requested == 0 {
        return Ok(BrkShape::Query);
    }
    if requested < base {
        return Err(SyscallError::InvalidArgs);
    }
    if requested > current_end {
        return Ok(BrkShape::Growth);
    }
    if requested == current_end {
        return Ok(BrkShape::NoOp);
    }
    let unmap_start = round_up_page(requested)?;
    let unmap_end = round_up_page(current_end)?;
    if unmap_start < unmap_end {
        Ok(BrkShape::ShrinkUnmapping {
            unmap_start,
            unmap_end,
        })
    } else {
        Ok(BrkShape::ShrinkWithinPage)
    }
}

/// THE brk transaction — every shape, one policy.
///
/// Returns the value the caller's first result lane carries: the current end for a query, the
/// requested break for everything else.
pub(crate) fn run_vm_brk_transaction<O: VmBrkOwners>(
    owners: &mut O,
    requested: usize,
) -> Result<usize, SyscallError> {
    use crate::kernel::syscall::validate_user_region;

    let tid = owners
        .caller_tid()
        .ok_or(SyscallError::from(KernelError::TaskMissing))?;
    // The group-leader rule comes FIRST, before the query, exactly as delivered: a non-leader is
    // refused even when it only asks.
    if !owners.is_group_leader(tid) {
        return Err(SyscallError::InvalidArgs);
    }

    if requested == 0 {
        // The query reads bounds that may not exist yet and reports 0 for that, rather than
        // refusing — the delivered `unwrap_or(0)`.
        let current_end = owners.brk_bounds(tid).map(|(_, end)| end).unwrap_or(0);
        owners.note_brk(BrkShape::Query, 0);
        return Ok(current_end);
    }

    validate_user_region(requested as u64, 1)?;
    let (base, current_end) = owners.brk_bounds(tid).ok_or(SyscallError::InvalidArgs)?;
    let shape = classify_brk(requested, base, current_end)?;

    let mut pages_unmapped = 0usize;
    if let BrkShape::ShrinkUnmapping {
        unmap_start,
        unmap_end,
    } = shape
    {
        let asid = owners
            .caller_asid(tid)
            .ok_or(SyscallError::from(KernelError::UserMemoryFault))?;
        pages_unmapped = owners
            .unmap_brk_range(asid, unmap_start, unmap_end)
            .map_err(SyscallError::from)?;
    }

    // The task-existence half of the bounds write, asked before the write exactly as the
    // delivered `set_task_brk_bounds` asks it.
    if !owners.task_exists(tid) {
        return Err(SyscallError::from(KernelError::TaskMissing));
    }
    owners
        .set_brk_bounds(tid, base, requested)
        .map_err(SyscallError::from)?;
    owners.note_brk(shape, pages_unmapped);
    Ok(requested)
}
