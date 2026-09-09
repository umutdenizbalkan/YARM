// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-XFER1 — THE transfer-release transaction (NR 4 `TransferRelease`).
//!
//! Stated once here so the broad and split adapters cannot come to disagree about which shape the
//! request is, what it validates, in what order it mutates, or what a refusal leaves behind.
//!
//! # What NR 4's ABI actually admits
//!
//! `arg(CAP)` is any `CapId` in the caller's own cnode — **a capability userspace holds**, of any
//! of the nine `CapObject` kinds. Releasing it is a full recursive revoke: the in-cspace
//! derivation tree, plus a delegated closure that crosses process boundaries to arbitrary depth,
//! plus each member's transfer-mapping teardown, memory refcount and reclaim, and notification
//! destruction with a waiter wake.
//!
//! That is emphatically **not** the provisional-capability cleanup subset. The rollback sites that
//! subset serves revoke a cap minted moments earlier in the same syscall and never handed out, so
//! their closure is provably empty and a 16-element stack array is sound. NR 4's root has been
//! handed out. Its closure is bounded only by the delegation-link table, exactly as the broad
//! `collect_delegated_descendants` is, so this transaction reserves on the heap — and reserves
//! BEFORE it mutates anything, so a failed reservation is a refusal that costs nothing.
//!
//! # Why there is a whole-range preflight
//!
//! The broad handler unmapped page by page and refused mid-loop: an unmapped page returned
//! `InvalidArgs` **after** every preceding page had been unmapped and its frame reclaimed, with
//! the capability still live and the registration still present. The caller was told the call
//! failed while part of its shared region was permanently gone. The revoke's own refusal was
//! worse — it landed after the entire range was destroyed.
//!
//! So every refusal is raised in phase V, from reads only. After phase V the request is
//! committed: the range is unmapped, the capability is revoked, the registration is removed and
//! the accounting is applied. Nothing is refused once anything has been destroyed.
//!
//! This is not a new restriction on what NR 4 accepts. The preflight refuses exactly the inputs
//! the broad handler already refused — an unregistered `(0,0)`, a misaligned or zero-length
//! explicit range, an overflowing range, a missing ASID, an unmapped page, an unresolvable
//! capability — and it refuses them *earlier*, before they can destroy anything.
//!
//! # Phases
//!
//! ```text
//! V  validate args, resolve the shape, resolve the asid, prove every page of the
//!    range is mapped and the capability resolves          rank 2 / rank 3 / rank 4 / rank 5 READS
//! J  reserve the revoke closure on the heap               no acquisition, before any mutation
//! U  unmap the whole range, two-phase, requester-stated   rank 5 → TLB (no lock) → rank 6
//! R  revoke the capability over the reserved closure      rank 4 (+3, +6, +2, +1)
//! S  remove the registration and account the release      rank 3
//! ```
//!
//! No two domain locks are ever held at once, and nothing is held while a shootdown is awaited.

use crate::kernel::capabilities::CapId;
use crate::kernel::ipc::ThreadId;
use crate::kernel::syscall::{KernelError, SyscallError};
use crate::kernel::vm::{Asid, PAGE_SIZE, VirtAddr};

/// Which of NR 4's two request shapes the caller asked for.
///
/// Both existed before this transaction and both are preserved exactly; naming them is what lets
/// the two adapters agree on which one they are servicing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum XferShape {
    /// `(base, len) == (0, 0)`: release the range the active-transfer registry recorded for this
    /// `(owner, cap)` pair. The authoritative shape — the one `RecvSharedV3` hands userspace a
    /// cleanup token for.
    Registered,
    /// An explicit page-aligned `(base, len)`. Preserved verbatim: it releases the range the
    /// caller names in the caller's OWN address space, whether or not the registry recorded it.
    /// Narrowing that would be a new refusal, which this transaction does not introduce.
    Explicit,
}

/// The validated request: what phase V resolved, and the only thing the commit phases read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct XferReleasePlan {
    pub(crate) shape: XferShape,
    pub(crate) owner: ThreadId,
    pub(crate) cap: CapId,
    pub(crate) asid: Asid,
    pub(crate) base: usize,
    pub(crate) map_len: usize,
}

/// U9-XFER2 §2 — what phase R's revoke actually did.
///
/// The previous shape discarded this (`let _ = owners.revoke_user_held_capability(..)`), which
/// meant a revoke that did not land was reported as a clean release of a range that had already
/// been destroyed. It is a value now so the transaction has to account for it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum XferRevokeOutcome {
    /// The complete teardown ran over the reserved closure.
    Revoked,
    /// The root slot was already gone when the commit reached it: someone else retired the
    /// capability between the reservation and here. The root revoke is the FIRST mutation of the
    /// teardown, so nothing was left half-done — but this call is not what retired the capability,
    /// and saying so is the difference between reporting and hiding.
    AlreadyRetired,
}

/// What the transaction did, for the adapter's telemetry and markers. Not an error channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum XferTxnEvent {
    /// Phase V refused, from reads only. Nothing was mutated.
    Refused { reason: XferRefusal },
    /// The whole request committed: `pages` unmapped, capability revoked, registration removed.
    Released { pages: usize, map_len: usize },
    /// The range was unmapped but at least one page's shootdown did not complete, so that frame
    /// was deliberately left unreclaimed — and phase R's object-level reclaim was suppressed for
    /// the same reason. The release still committed.
    ReleasedWithIncompleteShootdown { pages: usize, map_len: usize },
    /// The range was released, but the capability had already been retired by someone else before
    /// phase R reached it. Reported rather than folded into `Released`, because this call did not
    /// perform the revocation its caller asked for even though the postcondition holds.
    ReleasedAfterConcurrentRevoke { pages: usize, map_len: usize },
}

/// Why phase V refused. Every variant maps to the error the broad handler already produced for
/// the same input — this transaction adds no refusal of its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum XferRefusal {
    /// The caller is a kernel task, or has no user address space.
    NoUserAddressSpace,
    /// `(0,0)` named a pair the active-transfer registry does not hold.
    NotRegistered,
    /// An explicit range with zero length or a misaligned base.
    BadRange,
    /// `base + map_len` overflows.
    RangeOverflow,
    /// The caller's ASID could not be resolved.
    ///
    /// Carried for completeness and mapped to the error the broad handler names for it
    /// (`UserMemoryFault`), but **not reachable through `plan_transfer_release`** — and it was not
    /// reachable on the broad path either. `handle_transfer_release` guarded its `task_asid` call
    /// with `current_task_has_user_asid`, whose success is exactly what makes `task_asid` return
    /// `Some`; its own comment says so. `caller_with_user_asid` states that in one owner instead
    /// of two, which is why nothing produces this variant rather than something silently
    /// swallowing it.
    NoAsid,
    /// The caller has no process cnode.
    NoCnode,
    /// At least one page of the range is not mapped. The broad handler discovered this mid-unmap
    /// and refused with the whole prefix already destroyed; this refuses before touching anything.
    RangeNotFullyMapped,
    /// The capability does not resolve in the caller's cnode, or its closure could not be
    /// reserved. Both are pre-mutation.
    CapabilityUnresolvable,
}

impl From<XferRefusal> for SyscallError {
    /// The EXACT error the broad handler produced for each input, unchanged. `NoAsid` is the one
    /// that is not `InvalidArgs`: `handle_transfer_release` mapped a missing ASID to
    /// `KernelError::UserMemoryFault`.
    fn from(refusal: XferRefusal) -> Self {
        match refusal {
            XferRefusal::NoAsid => SyscallError::from(KernelError::UserMemoryFault),
            XferRefusal::NoCnode => SyscallError::Internal,
            XferRefusal::CapabilityUnresolvable => {
                SyscallError::from(KernelError::InvalidCapability)
            }
            XferRefusal::NoUserAddressSpace
            | XferRefusal::NotRegistered
            | XferRefusal::BadRange
            | XferRefusal::RangeOverflow
            | XferRefusal::RangeNotFullyMapped => SyscallError::InvalidArgs,
        }
    }
}

/// The owners NR 4 needs, in the order the phases use them.
///
/// Every method is one rank-local acquisition or a pure read. The broad adapter implements them
/// over `&mut KernelState`; the split adapter over `&SharedKernel` with the entering CPU stated,
/// so its unmap reaches the requester-stating shootdown owner rather than the ambient one.
pub(crate) trait XferReleaseOwners {
    /// U9-XFER2 §2 — the OWNED, identity-bearing revoke reservation this adapter produces in
    /// phase V and consumes in phase R.
    ///
    /// Opaque to the policy on purpose: what it holds is the adapter's business, but that it is
    /// *carried* is the policy's. The previous shape built a reservation during the preflight,
    /// dropped it, and rebuilt one after the range was already destroyed — which made the
    /// preflight a rehearsal rather than a reservation, and put an allocation that could fail on
    /// the far side of the mutation boundary.
    type RevokeReservation;

    /// rank 2 — the calling thread, or `None` if it is a kernel task / has no user ASID.
    fn caller_with_user_asid(&mut self) -> Option<(ThreadId, Asid)>;

    /// rank 3 — the registered `(base, len)` for this pair, if the registry holds one.
    fn registered_range(&mut self, owner: ThreadId, cap: CapId) -> Option<(VirtAddr, usize)>;

    /// rank 4 — the caller's process cnode.
    fn caller_cnode(&mut self) -> Option<crate::kernel::capabilities::CNodeId>;

    /// rank 5 — is this exact page mapped in `asid`? Used by the preflight, so the whole range's
    /// mappedness is known before anything is removed.
    fn page_is_mapped(&mut self, asid: Asid, virt: VirtAddr) -> bool;

    /// rank 4 — RESERVE the capability's revoke closure, from reads only.
    ///
    /// Called in phase V. The returned value is owned by the transaction and handed back to
    /// [`Self::revoke_reserved_capability`] in phase R, so the capacity the commit needs is
    /// acquired while a failure still costs nothing. `None` is a pre-mutation refusal — the root
    /// did not resolve, or the closure could not be allocated.
    fn reserve_capability_release(
        &mut self,
        cnode: crate::kernel::capabilities::CNodeId,
        cap: CapId,
    ) -> Option<Self::RevokeReservation>;

    /// rank 5 → TLB with NO lock held → rank 6 — unmap the whole range. Returns `false` if at
    /// least one page's shootdown did not complete, in which case that frame was deliberately
    /// left unreclaimed rather than recycled under a translation a remote CPU may still hold.
    fn unmap_whole_range(&mut self, asid: Asid, base: usize, map_len: usize) -> bool;

    /// rank 4 (+3, +6, +2, +1) — the full recursive revoke of a capability USERSPACE HOLDS, over
    /// the closure RESERVED IN PHASE V and handed back here. Not the provisional-cap subset.
    ///
    /// `backing_is_quarantined` carries phase U's shootdown verdict in. When the range's
    /// shootdown did not complete, its frame was deliberately left unreclaimed; without this the
    /// revoke's own reclaimer would find the pages already gone, conclude everything is
    /// acknowledged, and free the quarantined backing anyway.
    fn revoke_reserved_capability(
        &mut self,
        reservation: Self::RevokeReservation,
        backing_is_quarantined: bool,
    ) -> XferRevokeOutcome;

    /// rank 3 — remove the registry entry, if one exists. `false` simply means there was none,
    /// which the `Explicit` shape permits.
    fn remove_registration(&mut self, owner: ThreadId, cap: CapId) -> bool;

    /// rank 3 — the release accounting the broad handler applies when a registration was removed.
    fn account_release(&mut self, map_len: usize);

    /// Observability. Never an error channel.
    fn note(&mut self, event: XferTxnEvent);
}

/// Phase V — resolve the request shape and validate it, from READS ONLY.
///
/// Split out from the transaction so both adapters and the tests can drive the whole refusal set
/// without mutating anything.
pub(crate) fn plan_transfer_release<O: XferReleaseOwners>(
    owners: &mut O,
    cap: CapId,
    base_arg: usize,
    len_arg: usize,
) -> Result<(XferReleasePlan, O::RevokeReservation), XferRefusal> {
    let (owner, asid) = owners
        .caller_with_user_asid()
        .ok_or(XferRefusal::NoUserAddressSpace)?;

    let (shape, base, map_len) = if base_arg == 0 && len_arg == 0 {
        let (base, len) = owners
            .registered_range(owner, cap)
            .ok_or(XferRefusal::NotRegistered)?;
        (XferShape::Registered, base.0 as usize, len)
    } else {
        if len_arg == 0 || !base_arg.is_multiple_of(PAGE_SIZE) {
            return Err(XferRefusal::BadRange);
        }
        let rounded = crate::kernel::syscall::helpers::round_up_page(len_arg)
            .map_err(|_| XferRefusal::BadRange)?;
        (XferShape::Explicit, base_arg, rounded)
    };

    let end = base
        .checked_add(map_len)
        .ok_or(XferRefusal::RangeOverflow)?;

    let cnode = owners.caller_cnode().ok_or(XferRefusal::NoCnode)?;

    // The whole-range mappedness proof. The broad handler learned this one page at a time, DURING
    // the unmap, and refused with the prefix already destroyed. Proving it first is what makes
    // every NR 4 refusal harmless.
    let mut va = base;
    while va < end {
        if !owners.page_is_mapped(asid, VirtAddr(va as u64)) {
            return Err(XferRefusal::RangeNotFullyMapped);
        }
        va = va.saturating_add(PAGE_SIZE);
    }

    // …and the capability half of the same proof. This does not merely CHECK: it RESERVES, and
    // the reservation is handed back to the caller to carry into phase R. Checking and then
    // rebuilding would put the allocation that can fail on the far side of the unmap.
    let reservation = owners
        .reserve_capability_release(cnode, cap)
        .ok_or(XferRefusal::CapabilityUnresolvable)?;

    Ok((
        XferReleasePlan {
            shape,
            owner,
            cap,
            asid,
            base,
            map_len,
        },
        reservation,
    ))
}

/// THE transfer-release transaction. Phase V refuses or the whole request commits.
///
/// Returns the released length, which is what NR 4 puts in `ret0`.
pub(crate) fn run_transfer_release_transaction<O: XferReleaseOwners>(
    owners: &mut O,
    cap: CapId,
    base_arg: usize,
    len_arg: usize,
) -> Result<usize, SyscallError> {
    let (plan, reservation) = match plan_transfer_release(owners, cap, base_arg, len_arg) {
        Ok(prepared) => prepared,
        Err(reason) => {
            owners.note(XferTxnEvent::Refused { reason });
            return Err(SyscallError::from(reason));
        }
    };

    // ── Committed from here. Nothing below refuses. ─────────────────────────────────────────
    //
    // What makes that true is not optimism: phase V proved the whole range mapped and RESERVED
    // the revoke closure, so the two things that could still fail — a missing page and an
    // allocation — are already behind us. `reservation` is that owned capacity, carried in.
    let pages = plan.map_len / PAGE_SIZE;

    // U — the whole range at once, through the owner that releases rank 5 before the shootdown
    // and takes rank 6 only after it completes. `false` means at least one page's shootdown did
    // not complete and its frame was deliberately left unreclaimed.
    let all_acked = owners.unmap_whole_range(plan.asid, plan.base, plan.map_len);

    // R — the capability, over the closure reserved in phase V.
    //
    // The shootdown verdict goes WITH it. Without that, the revoke's own reclaimer would find the
    // pages already unmapped, conclude there was nothing left to acknowledge, and free the
    // quarantined backing that phase U had just refused to free.
    let revoke = owners.revoke_reserved_capability(reservation, !all_acked);

    // S — registry and accounting, in the broad handler's order: account only when a registration
    // was actually removed.
    if owners.remove_registration(plan.owner, plan.cap) {
        owners.account_release(plan.map_len);
    }

    // The event names what actually happened, in precedence order: a quarantined frame is the
    // most consequential outcome, then a capability somebody else retired, then the ordinary
    // clean release.
    owners.note(match (all_acked, revoke) {
        (false, _) => XferTxnEvent::ReleasedWithIncompleteShootdown {
            pages,
            map_len: plan.map_len,
        },
        (true, XferRevokeOutcome::AlreadyRetired) => XferTxnEvent::ReleasedAfterConcurrentRevoke {
            pages,
            map_len: plan.map_len,
        },
        (true, XferRevokeOutcome::Revoked) => XferTxnEvent::Released {
            pages,
            map_len: plan.map_len,
        },
    });
    Ok(plan.map_len)
}
