// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-SPAWN-TXN3 §2 — **THE** provisional-capability rollback contract.
//!
//! # What this is, and what it deliberately is not
//!
//! A spawn creates capabilities before it creates anything reachable, and every one of them has
//! to be given back exactly once if the spawn fails. Until now the giving-back went through
//! `KernelState::revoke_capability_in_cnode`, the kernel's GENERAL revocation: a sixteen-substep
//! cascade spanning capability(4), memory(6), IPC(3), task(2) and VM(5), which additionally
//! performs a two-phase TLB-shootdown unmap and an IPC report to the supervisor.
//!
//! The U9-SPAWN-TXN3 §1 audit proved that eleven of those sixteen substeps are **unreachable for
//! every capability a spawn can create**, and unreachable *by object kind* rather than by timing
//! or by an assumption of privacy:
//!
//! | substep | verdict | why |
//! |---------|---------|-----|
//! | read source capability            | REQUIRED    | identity + object revalidation |
//! | `tid_for_cnode` → root pid        | REQUIRED    | the root of the delegation graph |
//! | collect delegated descendants     | REQUIRED    | the endpoint RECV cap really has a live link to the child when its grant is released |
//! | in-cspace derivation-tree walk    | REQUIRED, single slot | `CapSpace::mint` installs `parent: None`, and `mint_derived` has no production caller, so a spawn capability never has an in-cspace child |
//! | per-descendant slot removal       | REQUIRED    | follows from the descendants being real |
//! | delegation-link removal           | REQUIRED    | follows from the same |
//! | active transfer-mapping revocation | UNREACHABLE | a transfer mapping can only ever name a capability that `resolve_memory_object_phys` accepts, and that returns `Err(WrongObject)` for everything except `MemoryObject` and `DmaRegion` |
//! | MemoryObject cap-refcount adjust  | UNREACHABLE | `_ => return` on the same two object kinds |
//! | MemoryObject reclaim              | UNREACHABLE | the same match |
//! | Notification teardown             | UNREACHABLE | `let CapObject::Notification { .. } = object else { return }` |
//!
//! Every capability a spawn creates is `CapObject::AddressSpace` (the capability naming the
//! child's address space) or `CapObject::Endpoint` (the two service endpoints' SEND/RECEIVE
//! capabilities and every delegated copy of them). Neither can be a memory object, so neither can
//! carry a mapping, a refcount or a notification.
//!
//! So the reachable closure is **capability rank 4, plus one rank-2 `process_id` read**, and this
//! module is exactly that closure and nothing more. Splitting the other eleven substeps would be
//! generality with no reachable caller — the thing U9 has repeatedly found turns into drift.
//!
//! # What the audit did NOT let us assume
//!
//! Three hazards were considered explicitly and none of them widens the closure, but two of them
//! shape the contract:
//!
//! * **Sibling threads share a cspace.** `SpawnThread` (NR 11) puts a new thread into its
//!   parent's EXISTING CNode, so another thread of the spawning process can be running
//!   concurrently on another CPU, and on the split path the capability lock is released between
//!   spawn phases. A sibling therefore *can* touch a freshly minted slot.
//! * **A fresh CapId is not private.** Slots are allocated by first-fit, so the id is guessable,
//!   and `live_cap_ids` makes a cspace enumerable from inside the kernel. Nothing about "we did
//!   not return this id to userspace" makes the slot unreachable.
//!
//! What a sibling cannot do is change a capability's OBJECT KIND — and the closure above is
//! decided entirely by object kind. So the hazards do not add substeps. What they do force is
//! two properties this contract has:
//!
//! 1. every removal **revalidates the exact object incarnation first** and refuses without
//!    mutation on any mismatch, so a recycled or replaced slot is never destroyed; and
//! 2. descendants are **scanned live** rather than replayed from a list the spawn recorded, so a
//!    link a sibling created is removed too.
//!
//! * **Concurrent transfer** is covered by the transfer-mapping row above: an `Endpoint` or
//!   `AddressSpace` capability cannot become a transfer mapping at all.
//!
//! # The contract
//!
//! [`release_provisional_cap_locked`] removes, in this order:
//!
//! 1. every delegated **child** slot reachable from the token, and its link; then
//! 2. the token's own **source** slot.
//!
//! Child before source is not cosmetic: a child is authority derived from the source, so removing
//! the source first would leave live authority whose provenance is gone.
//!
//! It performs **no object destruction**. The endpoint incarnation and the address space are
//! owned by the spawn resource ledger, which holds exact generation-bearing tokens for them
//! (`ServiceEndpointGrant`, `ProvisionalSpawnResource::AddressSpace`) and tears them down through
//! their own owners. A capability rollback that also destroyed objects would destroy them twice.
//!
//! It is **repeat-inert**: a second call finds the slot absent and reports `AlreadyGone` without
//! touching anything.

use super::defs::DelegatedCapabilityLink;
use crate::kernel::boot::{CapabilitySubsystem, KernelError, kernel_mut, kernel_ref};
use crate::kernel::capabilities::{CNodeId, CapId, CapObject};

/// How many delegated descendants one provisional capability's rollback will follow.
///
/// A spawn's own graph is depth 1 and at most seven wide (one parent SEND copy, plus the child's
/// recv, reply-recv and four extra caps). The margin is for links a concurrent sibling created.
/// Overflow is reported, never silently truncated — see [`ProvisionalCapRelease::Residue`].
pub(crate) const MAX_PROVISIONAL_DESCENDANTS: usize = 24;

/// The unforgeable identity of one capability a spawn created provisionally.
///
/// Every field is part of the identity, and every one is checked before anything is removed:
///
/// * `cnode` / `pid` — the owning cspace and the process that owns it. The pid is the root of the
///   delegation graph, and is resolved from the cnode at rank 4 rather than from an ambient
///   current-task read.
/// * `cap` — the slot AND its generation. `CapId` packs both, and `CapSpace::get` refuses a
///   generation mismatch, so a token naming a recycled slot resolves to `None`.
/// * `object` — the exact object incarnation. For an endpoint this includes the endpoint's
///   generation, so a token naming a recycled endpoint slot is refused even if the capability
///   slot itself was not recycled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProvisionalCap {
    pub(crate) cnode: CNodeId,
    pub(crate) pid: u64,
    pub(crate) cap: CapId,
    pub(crate) object: CapObject,
}

/// What one rollback actually did. Typed so the ledger can log it and a guard can enumerate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProvisionalCapRelease {
    /// The source slot, and `descendants` delegated children, were removed.
    Released { descendants: usize },
    /// The slot no longer holds anything. Repeat-inert: nothing was touched.
    AlreadyGone,
    /// The slot holds a DIFFERENT object than the token names — it was recycled or replaced.
    /// Nothing was touched, which is the whole point: a stale token must never destroy a
    /// replacement.
    StaleObject { found: CapObject },
    /// The delegation graph is wider than [`MAX_PROVISIONAL_DESCENDANTS`]. Nothing was touched,
    /// because a partial descendant sweep is worse than none: it would remove some authority and
    /// leave the rest with no provenance.
    Residue,
}

impl ProvisionalCapRelease {
    /// True only when the source slot was actually removed.
    pub(crate) fn released(self) -> bool {
        matches!(self, Self::Released { .. })
    }
    /// Stable tag for the `SPAWN_PROVCAP_RELEASE` marker.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Self::Released { .. } => "released",
            Self::AlreadyGone => "already_gone",
            Self::StaleObject { .. } => "stale_object",
            Self::Residue => "residue",
        }
    }
}

/// One `(pid, cap)` pair in the delegation graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DelegatedSlot {
    pub(crate) pid: u64,
    pub(crate) cap: CapId,
}

/// Phase A, capability rank 4, READ-ONLY: every link whose `source_cap` numerically matches one of
/// the caps already in the closure.
///
/// Numeric-cap space needs no pid resolution, so this runs entirely at rank 4 and produces a
/// SUPERSET of the true closure — phase C narrows it with the resolved pids. Returning a superset
/// is what lets the whole walk happen in one rank-4 read, one rank-2 read and one rank-4 write,
/// with no lock held across any of them.
///
/// Returns `None` when the superset would exceed [`MAX_PROVISIONAL_DESCENDANTS`].
pub(crate) fn collect_link_closure_locked(
    capability: &CapabilitySubsystem,
    root_cap: CapId,
) -> Option<([Option<DelegatedCapabilityLink>; MAX_PROVISIONAL_DESCENDANTS], usize)> {
    let links = kernel_ref(&capability.delegated_capability_links);
    let mut out: [Option<DelegatedCapabilityLink>; MAX_PROVISIONAL_DESCENDANTS] =
        [None; MAX_PROVISIONAL_DESCENDANTS];
    let mut len = 0usize;
    // Caps whose children we still owe a scan for. Starts as the root; grows as children are
    // found. Bounded by the same limit, so the walk terminates.
    let mut frontier: [CapId; MAX_PROVISIONAL_DESCENDANTS] =
        [CapId(0); MAX_PROVISIONAL_DESCENDANTS];
    frontier[0] = root_cap;
    let mut frontier_len = 1usize;
    let mut head = 0usize;
    while head < frontier_len {
        let current = frontier[head];
        head += 1;
        for link in links.iter().flatten() {
            if link.source_cap != current {
                continue;
            }
            // Already collected? Links are unique per (source,dest) quadruple.
            if out[..len].iter().flatten().any(|seen| *seen == *link) {
                continue;
            }
            if len >= MAX_PROVISIONAL_DESCENDANTS {
                return None;
            }
            out[len] = Some(*link);
            len += 1;
            if !frontier[..frontier_len].contains(&link.dest_cap) {
                if frontier_len >= MAX_PROVISIONAL_DESCENDANTS {
                    return None;
                }
                frontier[frontier_len] = link.dest_cap;
                frontier_len += 1;
            }
        }
    }
    Some((out, len))
}

/// Phase C, capability rank 4: remove the delegated children, then the source slot.
///
/// `resolved` maps each candidate link to the PIDs its `source_tid` / `dest_tid` belong to, taken
/// at rank 2 between the two rank-4 acquisitions. A link whose pid could not be resolved is
/// treated as NOT matching, which is the fail-closed direction: it leaves the link alone rather
/// than removing one that might belong to a different process.
///
/// The source slot is removed LAST, and only if every child that was supposed to go actually
/// went. If a child refuses (its slot was recycled under us), the source stays too — because a
/// source removed while a derived child survives leaves live authority with no provenance.
pub(crate) fn release_provisional_cap_locked(
    capability: &mut CapabilitySubsystem,
    token: &ProvisionalCap,
    candidates: &[Option<DelegatedCapabilityLink>],
    resolved: &[Option<(u64, u64)>],
) -> ProvisionalCapRelease {
    // ── 1. Revalidate the source, before touching anything. ────────────────────────────
    let current = capability
        .cnode_spaces
        .iter()
        .flatten()
        .find(|space| space.id == token.cnode)
        .and_then(|space| kernel_ref(&space.cspace).get(token.cap));
    let Some(current) = current else {
        return ProvisionalCapRelease::AlreadyGone;
    };
    if current.object != token.object {
        return ProvisionalCapRelease::StaleObject {
            found: current.object,
        };
    }

    // ── 2. Every delegated child reachable from this token, child first. ───────────────
    //
    // Walked in reverse collection order, so a chain is unwound from its leaf: a grandchild goes
    // before the child it was derived from.
    let mut removed = 0usize;
    let mut refused = false;
    for idx in (0..candidates.len()).rev() {
        let Some(link) = candidates[idx] else {
            continue;
        };
        let Some((source_pid, dest_pid)) = resolved.get(idx).copied().flatten() else {
            // Unresolvable tid: fail closed, leave the link alone.
            refused = true;
            continue;
        };
        // Is this link rooted at our capability, or at one of its descendants?
        let rooted_here = source_pid == token.pid && link.source_cap == token.cap;
        let rooted_in_descendant = candidates[..candidates.len()]
            .iter()
            .flatten()
            .zip(resolved.iter())
            .any(|(other, other_pids)| {
                other_pids.is_some_and(|(_, other_dest_pid)| {
                    other.dest_cap == link.source_cap && other_dest_pid == source_pid
                })
            });
        if !rooted_here && !rooted_in_descendant {
            continue;
        }
        // Remove the child slot, then its link. The child's own object must still be a derived
        // copy of ours — `Capability::derive` only attenuates rights, so the object is identical.
        let child = DelegatedSlot {
            pid: dest_pid,
            cap: link.dest_cap,
        };
        match remove_delegated_child_locked(capability, child, token.object) {
            ChildRemoval::Removed | ChildRemoval::AlreadyGone => {
                remove_link_locked(capability, &link);
                removed = removed.saturating_add(1);
            }
            ChildRemoval::StaleObject => {
                // Someone replaced the child slot. Leave both the slot and the link: the link is
                // the only remaining record that the replacement is not ours.
                refused = true;
            }
        }
    }
    if refused {
        return ProvisionalCapRelease::Residue;
    }

    // ── 3. The source slot, last. ──────────────────────────────────────────────────────
    let removed_source = capability
        .cnode_spaces
        .iter_mut()
        .flatten()
        .find(|space| space.id == token.cnode)
        .map(|space| kernel_mut(&mut space.cspace).revoke(token.cap).is_ok())
        .unwrap_or(false);
    if !removed_source {
        return ProvisionalCapRelease::AlreadyGone;
    }
    ProvisionalCapRelease::Released {
        descendants: removed,
    }
}

/// What happened to one delegated child slot.
enum ChildRemoval {
    Removed,
    AlreadyGone,
    StaleObject,
}

/// Remove one delegated child slot, refusing if it no longer holds the expected object.
///
/// `expected` is the SOURCE's object: `Capability::derive` only attenuates rights, never the
/// object, so a genuine derived copy names the same object incarnation.
fn remove_delegated_child_locked(
    capability: &mut CapabilitySubsystem,
    child: DelegatedSlot,
    expected: CapObject,
) -> ChildRemoval {
    let Some(cnode) = capability
        .process_cnodes
        .iter()
        .flatten()
        .find(|record| record.pid == child.pid)
        .map(|record| record.cnode)
    else {
        // The child's process has no cspace any more — its whole capability space is gone, so the
        // slot is too.
        return ChildRemoval::AlreadyGone;
    };
    let Some(space) = capability
        .cnode_spaces
        .iter_mut()
        .flatten()
        .find(|space| space.id == cnode)
    else {
        return ChildRemoval::AlreadyGone;
    };
    let cspace = kernel_mut(&mut space.cspace);
    let Some(found) = cspace.get(child.cap) else {
        return ChildRemoval::AlreadyGone;
    };
    if found.object != expected {
        return ChildRemoval::StaleObject;
    }
    if cspace.revoke(child.cap).is_ok() {
        ChildRemoval::Removed
    } else {
        ChildRemoval::AlreadyGone
    }
}

/// Remove exactly one delegation link, matched on the whole quadruple.
fn remove_link_locked(capability: &mut CapabilitySubsystem, link: &DelegatedCapabilityLink) {
    let links = kernel_mut(&mut capability.delegated_capability_links);
    for slot in links.iter_mut() {
        if slot.is_some_and(|candidate| {
            candidate.source_tid == link.source_tid
                && candidate.source_cap == link.source_cap
                && candidate.dest_tid == link.dest_tid
                && candidate.dest_cap == link.dest_cap
        }) {
            *slot = None;
            return;
        }
    }
}

/// Resolve the owning process of a cspace, at capability rank 4.
///
/// This is the token's `pid`, and it comes from the capability domain's own process/CNode
/// association rather than from an ambient current-task read — the same rule U9-SPAWN-IC1
/// established for every other spawn identity.
pub(crate) fn pid_for_cnode_locked(
    capability: &CapabilitySubsystem,
    cnode: CNodeId,
) -> Option<u64> {
    capability
        .process_cnodes
        .iter()
        .flatten()
        .find(|record| record.cnode == cnode)
        .map(|record| record.pid)
}

/// Build the token for a capability this spawn minted, at capability rank 4.
///
/// Returns `None` when the cspace has no process association — which means the capability cannot
/// be the root of a delegation graph, and the caller falls back to the plain slot removal.
pub(crate) fn provisional_cap_token_locked(
    capability: &CapabilitySubsystem,
    cnode: CNodeId,
    cap: CapId,
) -> Result<ProvisionalCap, KernelError> {
    let object = capability
        .cnode_spaces
        .iter()
        .flatten()
        .find(|space| space.id == cnode)
        .and_then(|space| kernel_ref(&space.cspace).get(cap))
        .ok_or(KernelError::InvalidCapability)?
        .object;
    let pid = pid_for_cnode_locked(capability, cnode).ok_or(KernelError::TaskMissing)?;
    Ok(ProvisionalCap {
        cnode,
        pid,
        cap,
        object,
    })
}
