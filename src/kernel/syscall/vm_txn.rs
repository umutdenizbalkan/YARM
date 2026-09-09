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
//! lock made the whole loop atomic. Off that lock it is not, and three obligations follow that
//! the in-lock shape cannot meet.
//!
//! ## 1. Failure semantics are REQUEST-wide, not chunk-wide
//!
//! The delivered rollback is `rollback_anon_map(kernel, asid, addr, mapped_end, ..)`, and `addr`
//! there is the ORIGINAL request address: a failure at any page unmaps, revokes and reclaims
//! everything the request had installed, from its base. Servicing a request as a sequence of
//! independently committing runs does NOT reproduce that — a failure in a later run leaves the
//! earlier runs installed, minted and accounted, which is silently accepted partial success.
//!
//! So the transaction has one failure domain: the whole request. Its journal is reserved for the
//! whole request BEFORE any mutation, because a `no_std` transaction must not depend on an
//! allocation succeeding in order to be able to roll back. The reservation adds no reachable
//! refusal — every page also needs its own memory-object slot from a fixed table, so a request
//! whose journal will not fit cannot get past phase R either, and reports the same error.
//!
//! ## 2. Mapping compensation must prove OWNERSHIP, not matching bytes
//!
//! `AddressSpace::map_page` REPLACES, and comparing a recorded `(va, phys)` on the way out
//! excludes neither a concurrent replacement that happens to reuse the same physical backing nor
//! an ABA reuse of the frame. There is no per-mapping identity token to appeal to instead.
//!
//! What is available is serialization, and it is only worth anything if it covers the install AND
//! the undo. So EVERY page-table write of the request — install, restore and removal — happens
//! inside ONE acquisition, and the capability mint happens BEFORE it. That ordering is forced,
//! not preferred: a mint placed after the install would have to undo the install on failure, by
//! which time the installing acquisition is gone, so the undo would run in a second acquisition
//! and could not prove that the page it removes is still the page it put there.
//!
//! The cost is that a provisional capability now exists across the address-space phase. That is
//! the deliberate trade. An escaped provisional capability is COMPENSABLE — the release below
//! re-establishes identity inside the one capability acquisition that removes the slot, and a cap
//! that has genuinely escaped is retained with a named cleanup owner. A mapping destroyed by a
//! rollback that could not prove ownership is not compensable at all.
//!
//! ## 3. A removed translation owes an acknowledgement before its backing is recycled
//!
//! Removing a PTE invalidates it locally; a remote CPU running the same address space may still
//! hold the translation. The delivered rollback knew this — it paired `unmap_page_phase1` with
//! `execute_tlb_shootdown_wait_plan` before the frame could return to the allocator. Every page
//! this transaction installs and then removes therefore completes its shootdown, with NO domain
//! lock held across the wait, before its capability, its object or its frame is released. A page
//! whose acknowledgement does not arrive is left unreclaimed rather than recycled under a
//! possibly-stale translation.
//!
//! The map reference is taken in the SAME acquisition as the install, and dropped only after the
//! removal is acknowledged, so there is no interval in which a page of this request is live in
//! the page table with no map reference — an interval a competing mapping operation could
//! otherwise observe, and whose displaced-frame reclaim would free backing this transaction still
//! owns.
//!
//! # What this module deliberately does NOT do
//!
//! It never reaches general capability revocation (`revoke_capability_in_cnode`'s
//! delegated-descendant closure, active-transfer-mapping unmap, notification destroy). The
//! provisional caps it releases are ones it minted and still exclusively owns; anything else is
//! left alone rather than torn down. And it introduces no new user-visible refusal: every input
//! the broad handlers accept is accepted here, with the same error for every input they reject.

extern crate alloc;

use crate::kernel::boot::KernelError;
use crate::kernel::capabilities::{CNodeId, CapId};
use crate::kernel::syscall::SyscallError;
use crate::kernel::vm::{Asid, Mapping, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};

/// One page of the request, and everything its rollback needs.
///
/// The journal is allocated for the WHOLE request before any mutation, so a failure at any page
/// can undo every page — there is no chunk boundary to leak a partial commit across. It is
/// bounded in practice by the memory-object table (`MAX_MEMORY_OBJECTS`): every page needs its own
/// object slot, so a request that would outgrow the journal cannot get past phase R either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageRecord {
    pub(crate) virt: VirtAddr,
    /// The object this transaction created for the page, and the frame it owns.
    pub(crate) object_id: u64,
    pub(crate) phys: PhysAddr,
    /// The capability minted for `object_id`, once phase M has run.
    pub(crate) cap: Option<CapId>,
    /// Set once the page's PTE has been written by phase I.
    pub(crate) installed: bool,
    /// Set while this transaction holds a PIN on `object_id` — a hold nothing outside the
    /// transaction can drop. Cleared exactly once, when the obligation it covers is settled.
    pub(crate) pinned: bool,
    /// The IDENTITY of the object backing `replaced`, recorded when this transaction pinned it
    /// inside the acquisition that displaced it. `Some` means the hold is held and not yet
    /// settled.
    ///
    /// It is an id, not a flag, because releasing the hold and reclaiming its backing must name
    /// the SAME object it pinned. A physical address alone would not: once the hold is dropped the
    /// object can be reclaimed and its frame reissued, so a later lookup by address could act on a
    /// completely different object that now sits there.
    pub(crate) displaced_pinned: Option<u64>,
    /// What the install displaced, recorded inside the acquisition that displaced it. On the
    /// failure path this is restored verbatim, in the SAME acquisition; on the success path its
    /// frame is accounted and then reclaimed after its shootdown.
    pub(crate) replaced: Option<Mapping>,
}

impl PageRecord {
    fn new(virt: VirtAddr, object_id: u64, phys: PhysAddr) -> Self {
        Self {
            virt,
            object_id,
            phys,
            cap: None,
            installed: false,
            pinned: false,
            displaced_pinned: None,
            replaced: None,
        }
    }

    /// The provisional frame view the capability-release owner takes.
    fn provisional(&self) -> Option<ProvisionalFrame> {
        self.cap.map(|cap| ProvisionalFrame {
            object_id: self.object_id,
            cap,
            phys: self.phys,
        })
    }
}

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

/// Every domain operation the mapping transaction performs. One method, one acquisition; no method
/// contains phase order, validation sequencing or rollback policy.
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

    /// rank 6 — take one frame and create its memory object. NO capability is minted here.
    ///
    /// The delivered RIGHTS check is evaluated at this point and with this error, against the
    /// rights the mint will carry rather than against a published capability. Same predicate,
    /// same position in the error precedence, no cap required.
    fn acquire_object(&mut self, flags: PageFlags) -> Result<(u64, PhysAddr), KernelError>;

    /// rank 6 — take this TRANSACTION's own hold on `object_id`, and report whether it was
    /// taken.
    ///
    /// A capability is not a hold: it is user-revocable, and a sibling revoking the last one
    /// makes the object reclaimable and its frame reusable while this transaction is still
    /// relying on the physical address it recorded. A pin is not revocable from userspace at all
    /// — `reclaim_memory_object_if_unreferenced` and `reclaim_memory_object_for_phys` both refuse
    /// while `pin_refcount != 0` — so it is the hold this transaction actually owns.
    ///
    /// Taken BEFORE any revocable authority is published, and released or transferred exactly
    /// once, when the obligation it covers is settled.
    fn pin_object(&mut self, object_id: u64) -> bool;

    /// rank 6 — release this transaction's hold, then let the object be reclaimed if that was its
    /// last reference of any kind. Called exactly once per successful [`Self::pin_object`].
    fn unpin_object(&mut self, object_id: u64);

    /// ONE rank-6 acquisition — release the hold this transaction took on a DISPLACED page's
    /// backing AND reclaim that exact object, together.
    ///
    /// Deliberately one operation, not an unpin followed by a reclaim: between two acquisitions a
    /// competing final-reference release could reclaim the now-unpinned object and the allocator
    /// could reissue its frame, after which a reclaim keyed on the physical address would act on
    /// whatever object had taken its place. Naming `object_id` and settling inside one acquisition
    /// removes both the window and the ambiguity.
    ///
    /// Returns whether the object was actually reclaimed.
    fn settle_displaced_hold(&mut self, object_id: u64, phys: PhysAddr) -> bool;

    /// rank 6 — release an object that no capability and no mapping ever referenced.
    ///
    /// Reachable ONLY from phase R's own failure path, and only for objects phase M has not yet
    /// minted a capability for. At that point nothing in the system can name the object: no slot
    /// was published and no PTE was written, so freeing its backing cannot take a frame another
    /// mapping now references. The owner asserts that precondition rather than assuming it.
    fn release_unminted_object(&mut self, object_id: u64);

    /// rank 4 — mint the caller's capability for an object this transaction created.
    ///
    /// Phase M, and it runs BEFORE the address space is touched. See the module header: a mint
    /// after the install would force the install's rollback into a second VM acquisition, and a
    /// rollback that cannot see the acquisition that installed cannot prove what it is undoing.
    fn mint_frame_cap(&mut self, object_id: u64, phys: PhysAddr) -> Result<CapId, KernelError>;

    /// ONE acquisition, VM rank 5 then memory rank 6 — install the WHOLE request, take the map
    /// reference on every frame, account every mapping it displaced, and on any failure restore
    /// every page it touched, all before releasing.
    ///
    /// This is the whole of the address-space phase. Nothing else in the transaction writes a PTE
    /// or a map refcount, so ownership of everything the failure path removes is established by
    /// serialization over the entire request rather than by comparing a virtual and physical
    /// address, which excludes neither a same-backing replacement nor an ABA frame reuse.
    ///
    /// On failure returns the index that failed; `records[..]` still describes what happened, so
    /// the caller knows which pages were installed and removed and therefore owe a shootdown.
    fn install_and_account(
        &mut self,
        asid: Asid,
        flags: PageFlags,
        records: &mut [PageRecord],
    ) -> Result<(), (usize, KernelError)>;

    /// NO LOCK — complete the required TLB shootdown for `virt` in `asid`. Returns `false` when a
    /// remote acknowledgement was not obtained, in which case the caller must skip the reclaim.
    fn complete_shootdown(&mut self, asid: Asid, virt: VirtAddr) -> bool;

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
    /// Phase R — no capability and no mapping exists for anything this phase acquired.
    FrameAlloc,
    /// Phase M — capabilities exist, the address space has not been touched.
    CapabilityMint,
    /// Phase I — the address space phase restored itself before releasing; what remains is the
    /// shootdown its removals owe, and the capabilities and objects it no longer needs.
    PageTableUpdate,
    /// The journal itself could not be reserved. Nothing was acquired, minted or installed.
    JournalReserve,
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

/// Release every capability in `records` that is still this transaction's to release, and report
/// `(released, retained)`.
///
/// A retained frame is one a sibling took ownership of while this transaction was in flight; its
/// capability and its object stay exactly as they are, which is the only correct outcome — the
/// frame is still referenced, so it is not leaked, and removing it would destroy another
/// transaction's resource. Its cleanup owner is `revoke_capability_in_cnode`, which process
/// teardown drives once per live capability.
fn release_capabilities<O: VmMapOwners>(
    owners: &mut O,
    cnode: Option<CNodeId>,
    records: &[PageRecord],
    settled_only: bool,
) -> (usize, usize) {
    let eligible =
        |record: &&PageRecord| record.cap.is_some() && !(settled_only && record.installed);
    let minted = records.iter().filter(eligible).count();
    let Some(cnode) = cnode else {
        // No cspace to release into. Nothing was minted there either.
        return (0, minted);
    };
    let mut released = 0usize;
    let mut retained = 0usize;
    // Iterated IN PLACE over the journal reserved before any mutation: compensation allocates
    // nothing. An intermediate collection here would be an infallible allocation on the failure
    // path, which is exactly what the journal exists to avoid.
    for record in records.iter().filter(eligible) {
        let Some(frame) = record.provisional() else {
            continue;
        };
        match owners.release_provisional_cap(cnode, frame) {
            ProvisionalReleaseOutcome::Released => {
                owners.account_released_cap(frame);
                released += 1;
            }
            // Someone else's retirement already did this frame's accounting, or someone derived
            // from the slot while this transaction was in flight. Either way it is not ours.
            ProvisionalReleaseOutcome::NotOurs | ProvisionalReleaseOutcome::Derived => {
                retained += 1;
            }
        }
    }
    (released, retained)
}

/// Every page phase I installed and then removed owes a shootdown before its backing may return
/// to the allocator. Complete them with NO lock held, and settle this transaction's hold on each
/// page whose acknowledgement arrives.
///
/// A page whose acknowledgement does NOT arrive keeps its pin. The pin, not the capability, is the
/// durable owner of that unsettled obligation: a capability can be revoked by any sibling, and the
/// instant the last one goes the object becomes reclaimable and its frame reusable under a
/// translation a remote CPU may still hold. A pin cannot be dropped from outside this transaction
/// at all.
///
/// Stated precisely, because the difference matters: a retained pin is INDEFINITE QUARANTINE, not
/// deferred retirement. It guarantees the frame is never reissued, and nothing more. There is no
/// completion record for a shootdown that never arrived and no consumer that would release the pin
/// if one arrived later, so no owner ever retires the page — the object and its frame are held for
/// the lifetime of the kernel. That is the deliberate fail-closed choice (a quarantined frame is
/// strictly safer than one reissued under a live remote translation), and it must not be described
/// as anything else.
///
/// On the ports where it could arise: it cannot on AArch64, whose invalidation is a completed
/// architectural broadcast, nor on RISC-V, where no secondary hart ever dispatches a user task and
/// so the remote target set is empty by the port's own dispatch rules. On x86_64 it requires a
/// target that took the shootdown IPI but never published its acknowledgement.
fn settle_rollback_shootdowns<O: VmMapOwners>(
    owners: &mut O,
    asid: Asid,
    records: &mut [PageRecord],
) -> usize {
    let mut unacknowledged = 0usize;
    for record in records.iter_mut() {
        if !record.installed {
            continue;
        }
        if owners.complete_shootdown(asid, record.virt) {
            // The translation is retired everywhere it could have been cached, so this page's
            // backing may now be released. There is no map reference to drop: pass B of the
            // address-space phase is the only thing that takes one, and it runs only once the
            // whole request has installed — so a page reached by this path never had one.
            record.installed = false;
        } else {
            unacknowledged += 1;
        }
    }
    unacknowledged
}

/// Drop this transaction's hold on every page whose obligation is settled — that is, every page
/// that no longer owes a shootdown acknowledgement. Called exactly once per page that took one.
fn release_settled_pins<O: VmMapOwners>(owners: &mut O, records: &mut [PageRecord]) {
    for record in records.iter_mut() {
        if record.pinned && !record.installed {
            owners.unpin_object(record.object_id);
            record.pinned = false;
        }
    }
}

/// THE anonymous-mapping transaction: NR 3 and NR 13, one policy, one request.
///
/// | phase | acquisition | on failure |
/// |---|---|---|
/// | J: reserve the whole-request journal | none | nothing acquired, nothing mutated |
/// | V: validate, resolve target, guard page | rank 4 / rank 5 reads | nothing acquired |
/// | R: acquire every object and frame, and PIN each one | rank 6 | unpin and release exactly the objects acquired so far |
/// | M: mint every capability | rank 4 | release the capabilities minted so far, then unpin and release every object |
/// | I: install the whole request, take map references, account and PIN every displaced page | ONE rank 5 → rank 6 | the acquisition restores every page it touched BEFORE releasing; then shootdown, then unpin, then capabilities, then objects |
/// | W: required shootdown acknowledgements | none | — |
/// | S: unpin and reclaim displaced backing; release this request's own pins | rank 6 | — |
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
    let pages = args.map_len / PAGE_SIZE;

    // ── Phase J: the journal for the WHOLE request, reserved before any mutation.
    //
    // The delivered rollback covers `[addr, mapped_end)` — measured from the REQUEST base, not
    // from any chunk — so a failure at any page must be able to undo every page. That needs a
    // record per page, and a `no_std` transaction must not depend on an allocation succeeding
    // *after* it has begun mutating. Reserving here is what lets every later compensation step
    // run in place, allocating nothing.
    //
    // This reservation CAN fail, and the honest statement of when is not "only above the
    // object-table limit". The journal comes from the kernel heap and the frames come from the
    // frame allocator; they are different pools, so heap exhaustion below the object-table limit
    // is possible in principle — a request of a few dozen pages needs about a kilobyte of heap,
    // and a kernel heap that cannot supply that is already in trouble for other reasons. What the
    // object-table limit does bound is the journal's WORST CASE: a request can never install more
    // pages than there are object slots, so the reservation is bounded by a fixed boot constant
    // rather than by the request.
    //
    // What matters for correctness is not that the reservation cannot fail, but that a failure
    // costs nothing: it happens before any object, capability, mapping, pin or frame has been
    // touched, so the refusal leaves the system byte-for-byte as it found it, and reports the same
    // error phase R reports when the object table is what runs out.
    let mut records: alloc::vec::Vec<PageRecord> = alloc::vec::Vec::new();
    if records.try_reserve_exact(pages).is_err() {
        owners.note(VmTxnEvent::RolledBack {
            reason: VmRollbackReason::JournalReserve,
            released: 0,
            retained: 0,
        });
        return Err(SyscallError::from(KernelError::MemoryObjectFull));
    }

    // ── Phase R: every frame and its object, for the whole request, each PINNED as it is
    // acquired.
    //
    // The pin is the hold this TRANSACTION owns. A capability is not one: it is user-revocable,
    // and a sibling revoking the last capability for an object makes it reclaimable and its frame
    // reusable — while this transaction is still about to write `record.phys` into a page table.
    // `reclaim_memory_object_if_unreferenced` and `reclaim_memory_object_for_phys` both refuse
    // while `pin_refcount != 0`, and nothing reachable from userspace can decrement it, so the
    // pin holds the backing across every later phase no matter what happens to the capability.
    //
    // It is taken here, BEFORE phase M publishes any revocable authority at all.
    for i in 0..pages {
        match owners.acquire_object(args.flags) {
            Ok((object_id, phys)) => {
                let mut record = PageRecord::new(
                    VirtAddr((args.addr + i * PAGE_SIZE) as u64),
                    object_id,
                    phys,
                );
                record.pinned = owners.pin_object(object_id);
                if !record.pinned {
                    // The hold is REQUIRED, not best-effort. Without it nothing keeps this object
                    // alive across the phases that follow, so `record.phys` could name recycled
                    // backing by the time the install writes it into a page table. A hold that
                    // cannot be taken must therefore fail the request — never silently authorize
                    // continued use of the journal's physical address.
                    owners.release_unminted_object(object_id);
                    for index in 0..records.len() {
                        if records[index].pinned {
                            owners.unpin_object(records[index].object_id);
                            records[index].pinned = false;
                        }
                        owners.release_unminted_object(records[index].object_id);
                    }
                    owners.note(VmTxnEvent::RolledBack {
                        reason: VmRollbackReason::FrameAlloc,
                        released: records.len(),
                        retained: 0,
                    });
                    return Err(SyscallError::from(KernelError::MemoryObjectMissing));
                }
                records.push(record);
            }
            Err(e) => {
                for index in 0..records.len() {
                    if records[index].pinned {
                        owners.unpin_object(records[index].object_id);
                        records[index].pinned = false;
                    }
                    owners.release_unminted_object(records[index].object_id);
                }
                owners.note(VmTxnEvent::RolledBack {
                    reason: VmRollbackReason::FrameAlloc,
                    released: records.len(),
                    retained: 0,
                });
                return Err(SyscallError::from(e));
            }
        }
    }
    owners.note(VmTxnEvent::FramesAcquired { count: pages });

    // ── Phase M: every capability, for the whole request, and BEFORE the address space is
    // touched.
    //
    // The order is forced, not preferred. A mint after the install would have to undo the install
    // on failure, and the install's acquisition has by then been released — so the undo would run
    // in a SECOND acquisition, unable to prove that what it is removing is still what it put
    // there. A competing mapping operation on another CPU may have replaced any of those pages in
    // the interval, possibly with the very same physical backing, so no comparison of virtual and
    // physical address could distinguish the two. Minting first keeps every page-table write and
    // every page-table undo inside one acquisition.
    //
    // A provisional capability is not private — every thread of a process shares one CNode, and
    // Fork inherits by enumerating the parent's cspace — so this order does widen the window in
    // which a sibling can capture, or revoke, a slot this transaction has not yet returned. The
    // pin taken in phase R is what makes that safe rather than merely unlikely: whatever a sibling
    // does to the capability, the object and its frame are held.
    for i in 0..pages {
        let (object_id, phys) = (records[i].object_id, records[i].phys);
        match owners.mint_frame_cap(object_id, phys) {
            Ok(cap) => records[i].cap = Some(cap),
            Err(e) => {
                // Nothing is installed, so there is no page-table work and no shootdown to owe.
                let (released, retained) =
                    release_capabilities(owners, cnode, &records[..i], false);
                for index in 0..records.len() {
                    if records[index].pinned {
                        owners.unpin_object(records[index].object_id);
                        records[index].pinned = false;
                    }
                }
                for index in i..records.len() {
                    owners.release_unminted_object(records[index].object_id);
                }
                owners.note(VmTxnEvent::RolledBack {
                    reason: VmRollbackReason::CapabilityMint,
                    released,
                    retained,
                });
                return Err(SyscallError::from(e));
            }
        }
    }

    // ── Phase I: ONE acquisition — VM rank 5, then memory rank 6 nested underneath it, which is
    // the legal direction — installs the whole request, takes the map reference on every frame,
    // and accounts and PINS every mapping it displaced. On any failure it restores every page it
    // touched BEFORE releasing.
    //
    // Because install, accounting and rollback all happen inside this one acquisition, and
    // because it spans the WHOLE request rather than a chunk of it, two claims hold that could
    // not hold otherwise: every page the failure path removes is provably the page this call
    // installed, and there is no interval in which a page of this request is live in the page
    // table with no map reference for another transaction to observe.
    if let Err((failed_index, e)) = owners.install_and_account(asid, args.flags, &mut records) {
        let _ = failed_index;
        // The address space is already back to its pre-transaction state, including at the page
        // that refused: `AddressSpace::map_page` restores a predecessor it broke before returning
        // an error, so the pages this call actually changed are exactly the ones it marked
        // `installed`. What remains is what the acquisition could not do — wait for the
        // acknowledgements its removals owe, and then let go of what this request no longer needs.
        let unacknowledged = settle_rollback_shootdowns(owners, asid, &mut records);
        // Order matters, and it is the inverse of the order the holds were taken in. The pin goes
        // first for every SETTLED page, because a capability release that finds the object
        // unreferenced must be able to reclaim it; a page still owing an acknowledgement keeps its
        // pin, and that pin — not the capability it also still holds — is the durable owner of
        // the obligation.
        release_settled_pins(owners, &mut records);
        let (released, retained) = release_capabilities(owners, cnode, &records, true);
        for index in 0..records.len() {
            if !records[index].installed && records[index].cap.is_none() {
                owners.release_unminted_object(records[index].object_id);
            }
        }
        owners.note(VmTxnEvent::RolledBack {
            reason: VmRollbackReason::PageTableUpdate,
            released,
            retained: retained + unacknowledged,
        });
        return Err(SyscallError::from(e));
    }
    owners.note(VmTxnEvent::Installed { count: pages });

    // ── Phase W and S: the request has committed. Retire whatever it displaced — shootdown
    // BEFORE reclaim, with NO domain lock held across the wait — and then let go of this
    // request's own holds, which the map references taken in phase I have now replaced.
    for index in 0..records.len() {
        let Some(old) = records[index].replaced else {
            continue;
        };
        if owners.complete_shootdown(asid, records[index].virt) {
            // ONE rank-6 operation: release this transaction's hold and reclaim that exact
            // object, named by identity. Reached only once the translation is provably gone, so
            // it is the single point at which the displaced backing becomes reusable — and
            // because the release and the reclaim cannot be separated, no competing
            // final-reference release can slip in between them and have this transaction mutate
            // whatever replaced the object it pinned.
            if let Some(object_id) = records[index].displaced_pinned.take() {
                owners.settle_displaced_hold(object_id, old.phys);
            }
        }
        // No acknowledgement: the pin stays, and with it the guarantee that no other reclaimer
        // can hand this frame out while a remote translation may still reach it.
    }
    for index in 0..records.len() {
        if records[index].pinned {
            // The page is installed and its map reference is taken, so the map reference is now
            // the hold. Released exactly once, here.
            owners.unpin_object(records[index].object_id);
            records[index].pinned = false;
        }
    }
    Ok((args.addr, args.map_len))
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
