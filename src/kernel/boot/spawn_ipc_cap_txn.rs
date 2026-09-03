// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-SPAWN-IC1 — the rank-local endpoint and capability bodies a spawn's service plumbing is
//! built out of.
//!
//! # The identity ledger this implements
//!
//! The shared NR 23 / NR 29 transaction creates two service endpoints and delegates one
//! capability. Every resource, by exact identity:
//!
//! | resource | identity | owner rank | becomes externally visible when |
//! |---|---|---|---|
//! | endpoint incarnation | `(index, generation)` in `endpoints[index]` / `endpoint_generations[index]`, mode `Buffered`, depth 8 | IPC 3 | a capability naming `(index, generation)` reaches a cspace some task can invoke |
//! | send capability | `CapId` in the SPAWNER's CNode, object `Endpoint{index, generation}`, rights `SEND` | capability 4 | the mint itself |
//! | receive capability | same CNode and object, rights `RECEIVE` | capability 4 | the mint itself |
//! | parent delegation | a `SEND` capability in the PARENT's CNode plus a `DelegatedCapabilityLink` | capability 4 | the mint into the parent's cspace |
//! | child delegations | `RECEIVE` caps minted into the CHILD's CNode | capability 4 | after the commit — not this module's |
//!
//! The exact inverse of each: revoke the capability through the capability owner (which cascades
//! to delegated descendants, so revoking the send capability also removes the parent's copy), then
//! remove the endpoint incarnation — but only if it is still the same incarnation and still
//! unpublished.
//!
//! # The defect this repairs
//!
//! `create_endpoint_with_mode` installed the endpoint under rank 3, released it, then minted two
//! capabilities under rank 4. Its own comment stated why — "capability rank 4 > ipc rank 3;
//! acquiring both simultaneously would invert lock order" — and that reasoning is correct about
//! ORDER but was solved by giving up ATOMICITY. A `CapabilityFull` on either mint returned an
//! error with the endpoint already installed, named by nothing and removed by nobody. The spawn
//! ledger could not compensate either: it only learns the endpoint index on success.
//!
//! Holding rank 3 and then rank 4 is not an inversion — it is the legal order — and
//! [`KernelState::with_ipc_then_capability_mut`] takes them that way, so the pair is one
//! transaction with one internal rollback.
//!
//! # What is rank-local here, and what deliberately is not
//!
//! CREATION is fully rank-local: slot selection, generation bump, endpoint construction, CNode
//! capacity check, and both mints touch nothing but `IpcSubsystem` and `CapabilitySubsystem`.
//!
//! DESTRUCTION of a LIVE endpoint is not, and cannot be: `destroy_endpoint` wakes a stranded
//! receiver (scheduler rank 1 + task rank 2) and settles orphaned senders' transfer envelopes
//! (memory rank 6). That work is real and stays. It is owed only by an endpoint that has been
//! published — one that acquired a waiter, a parked sender or an IRQ route. A provisional endpoint
//! that a failed spawn is giving back has none of those, and
//! [`remove_unpublished_endpoint_locked`] refuses rather than guesses: it checks all three, and
//! checks that the incarnation is still the one the token names.

use super::defs::{CapabilitySubsystem, IpcSubsystem};
use super::{KernelError, KernelState, kernel_mut, kernel_ref, store_kernel_value};
use crate::kernel::capabilities::{CNodeId, CapId, CapObject, CapRights, Capability};
use crate::kernel::ipc::{Endpoint, EndpointMode};

/// Everything the endpoint transaction needs, all of it settled before rank 3 is acquired.
///
/// `owner_cnode` in particular: the old path called `current_task_cnode()` from inside the mint,
/// an ambient read of task rank 2 made while deciding what to do with rank 4. Here the caller
/// resolves the identity first and passes it, so the body has no ambient anything.
pub(crate) struct ServiceEndpointRequest {
    /// The cspace both capabilities are minted into. Resolved under task rank 2 by the caller.
    pub(crate) owner_cnode: CNodeId,
    pub(crate) max_depth: usize,
    pub(crate) mode: EndpointMode,
    /// Capacity limits, read from the runtime capacity config before any owner is acquired.
    pub(crate) max_endpoints: usize,
    pub(crate) cnode_limits: CnodeGrowthLimits,
}

/// What one endpoint transaction produced, and what a rollback needs to give it back.
///
/// Identity-bearing in the way that matters: it carries the GENERATION, not just the index. The
/// spawn ledger's `ProvisionalSpawnResource::Endpoint(usize)` carries only an index, so a stale
/// unwind naming a recycled slot would destroy a REPLACEMENT endpoint. This token cannot express
/// that mistake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ServiceEndpointGrant {
    pub(crate) endpoint_index: usize,
    pub(crate) endpoint_generation: u64,
    pub(crate) owner_cnode: CNodeId,
    pub(crate) send_cap: CapId,
    pub(crate) recv_cap: CapId,
}

/// How far a CNode may be grown, read from the runtime capacity config BEFORE any owner is
/// acquired and normalized by the same authority the broad path uses
/// (`KernelState::normalize_requested_cnode_slots`).
///
/// Carried as a parameter rather than read inside the rank-4 body, because the config lives in the
/// boot-config domain and reading it there would be another out-of-rank reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CnodeGrowthLimits {
    pub(crate) slot_capacity: usize,
    pub(crate) max_total_cnode_slots: usize,
}

/// Mint one capability into `cnode`, under capability rank 4 and nothing else.
///
/// This is `mint_capability_in_cnode` minus its two out-of-rank steps: the ambient
/// `current_task_cnode()` its callers used to reach it through, and the
/// `adjust_memory_object_cap_refcount` tail, which is memory rank 6. The tail is a NO-OP for every
/// object except `MemoryObject` and `DmaRegion`, so an endpoint capability needs none — and a
/// caller minting one of those two through this body owes the adjustment after releasing rank 4.
/// [`delegate_capability_locked`] returns exactly that fact for that reason.
pub(crate) fn mint_in_cnode_locked(
    capability: &mut CapabilitySubsystem,
    cnode: CNodeId,
    cap: Capability,
    slot_capacity: usize,
    max_total_cnode_slots: usize,
) -> Result<CapId, KernelError> {
    KernelState::ensure_cnode_space_locked(
        capability,
        cnode,
        slot_capacity,
        max_total_cnode_slots,
    )?;
    capability
        .cnode_spaces
        .iter_mut()
        .flatten()
        .find(|space| space.id == cnode)
        .map(|space| kernel_mut(&mut space.cspace))
        .ok_or(KernelError::TaskMissing)?
        .mint(cap)
        .map_err(|_| KernelError::CapabilityFull)
}

/// Remove one capability slot, under capability rank 4 and nothing else.
///
/// Deliberately the NARROW form: it clears the slot and does not run the delegation cascade, the
/// transfer-mapping revocation, the MemoryObject refcount or the notification teardown that
/// `revoke_capability_in_cnode` performs. Every one of those reaches another rank, and none is
/// owed by a capability that was minted moments ago inside this transaction and has never been
/// delegated, transferred or object-linked. A capability that HAS been delegated is released by
/// the provisional-capability rollback instead — see
/// [`crate::kernel::boot::provisional_cap::release_provisional_cap_locked`], which removes a
/// delegated child and its link before the source slot.
fn revoke_fresh_cap_locked(
    capability: &mut CapabilitySubsystem,
    cnode: CNodeId,
    cap: CapId,
) -> bool {
    capability
        .cnode_spaces
        .iter_mut()
        .flatten()
        .find(|space| space.id == cnode)
        .map(|space| kernel_mut(&mut space.cspace).revoke(cap).is_ok())
        .unwrap_or(false)
}

/// THE service-endpoint transaction: an endpoint incarnation and the two capabilities naming it,
/// created together or not at all.
///
/// Steps, in order:
///
/// 1. **validate the exact target CNode identity and capacity** — `ensure_cnode_space_locked` is
///    reached by the first mint, and a CNode that cannot be created or grown fails there before
///    any capability exists;
/// 2. **allocate the endpoint incarnation** — free slot, generation bump, object install;
/// 3. **mint its capabilities with exact rights** — `SEND` then `RECEIVE`, both naming
///    `(index, generation)`;
/// 4. **return the identity-bearing token**.
///
/// On any failure the state is restored before the owners are released, in reverse order: the
/// minted capabilities are removed from the CNode, the endpoint slot is cleared, and the
/// generation is put back to the value it held on entry. Putting the generation back is the part
/// that is easy to get wrong: leaving it bumped would be "safe" but would silently consume
/// incarnations on every failure, and the census this transaction is measured against would drift.
pub(crate) fn provision_service_endpoint_locked(
    ipc: &mut IpcSubsystem,
    capability: &mut CapabilitySubsystem,
    request: &ServiceEndpointRequest,
) -> Result<ServiceEndpointGrant, KernelError> {
    // ── 2. The endpoint incarnation. ────────────────────────────────────────────────────
    let slot = ipc
        .endpoints
        .iter()
        .take(request.max_endpoints)
        .position(Option::is_none)
        .ok_or(KernelError::EndpointFull)?;
    let previous_generation = ipc.endpoint_generations[slot];
    let mut generation = previous_generation.wrapping_add(1);
    if generation == 0 {
        generation = 1;
    }
    let endpoint = Endpoint::new_with_mode(request.max_depth, request.mode)
        .map_err(|_| KernelError::WrongObject)?;
    ipc.endpoint_generations[slot] = generation;
    ipc.endpoints[slot] = Some(store_kernel_value(endpoint));

    // Restore the endpoint half exactly: slot empty, generation back to what it was.
    let unwind_endpoint = |ipc: &mut IpcSubsystem| {
        ipc.endpoints[slot] = None;
        ipc.endpoint_generations[slot] = previous_generation;
    };

    // ── 1 + 3. The CNode capacity check, then the two capabilities. ─────────────────────
    let object = CapObject::Endpoint {
        index: slot,
        generation,
    };
    let send_cap = match mint_in_cnode_locked(
        capability,
        request.owner_cnode,
        Capability::new(object, CapRights::SEND),
        request.cnode_limits.slot_capacity,
        request.cnode_limits.max_total_cnode_slots,
    ) {
        Ok(cap) => cap,
        Err(err) => {
            unwind_endpoint(ipc);
            crate::yarm_log!(
                "SPAWN_EP_TXN_FAIL phase=send_cap slot={} cnode={} err={:?}",
                slot,
                request.owner_cnode.0,
                err
            );
            return Err(err);
        }
    };
    let recv_cap = match mint_in_cnode_locked(
        capability,
        request.owner_cnode,
        Capability::new(object, CapRights::RECEIVE),
        request.cnode_limits.slot_capacity,
        request.cnode_limits.max_total_cnode_slots,
    ) {
        Ok(cap) => cap,
        Err(err) => {
            let revoked = revoke_fresh_cap_locked(capability, request.owner_cnode, send_cap);
            unwind_endpoint(ipc);
            crate::yarm_log!(
                "SPAWN_EP_TXN_FAIL phase=recv_cap slot={} cnode={} send_cap_revoked={} err={:?}",
                slot,
                request.owner_cnode.0,
                u8::from(revoked),
                err
            );
            return Err(err);
        }
    };

    crate::yarm_log!(
        "SPAWN_EP_TXN_OK slot={} generation={} cnode={} send_cap={} recv_cap={}",
        slot,
        generation,
        request.owner_cnode.0,
        send_cap.0,
        recv_cap.0
    );
    Ok(ServiceEndpointGrant {
        endpoint_index: slot,
        endpoint_generation: generation,
        owner_cnode: request.owner_cnode,
        send_cap,
        recv_cap,
    })
}

/// Why an unpublished-endpoint removal did or did not happen. Exhaustive, so a caller cannot
/// mistake "already gone" for "still there and in use".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndpointRemoval {
    /// The incarnation the token names was still installed, still unpublished, and is now gone.
    Removed,
    /// The slot no longer holds this incarnation: it was already removed, or has been reused by a
    /// later endpoint. Either way this token owns nothing, and touching the slot would destroy
    /// someone else's. Inert.
    Stale,
    /// The incarnation is still installed but has become externally visible — a blocked receiver,
    /// a parked sender, or an IRQ route names it. Removing it here would strand them; that is
    /// `destroy_endpoint`'s job and it needs ranks this body does not hold.
    Published,
}

/// Remove an endpoint incarnation that was never published, under IPC rank 3 and nothing else.
///
/// Three conditions, all checked rather than assumed:
///
/// 1. **the incarnation still matches** — `endpoints[index]` is occupied AND
///    `endpoint_generations[index]` equals the token's generation. An index alone is not an
///    identity: slots are recycled, and a stale token naming a recycled slot must not destroy the
///    endpoint that now lives there;
/// 2. **no receiver waits on it** — `endpoint_waiters[index]` is empty. A waiter would have to be
///    woken, which is scheduler and task work;
/// 3. **no sender is parked on it and no IRQ routes to it** — a parked sender may own a transfer
///    envelope that has to be settled (memory rank 6), and an IRQ route is an external reference.
///
/// A provisional endpoint from a failed spawn satisfies all three by construction: it was created
/// moments ago, no task has ever been given a capability to receive on it, and nothing routes to
/// it. If any check fails this returns without mutating, and the caller must use the live teardown.
pub(crate) fn remove_unpublished_endpoint_locked(
    ipc: &mut IpcSubsystem,
    index: usize,
    generation: u64,
) -> EndpointRemoval {
    if ipc.endpoints.get(index).map(Option::is_some) != Some(true)
        || ipc.endpoint_generations.get(index).copied() != Some(generation)
    {
        return EndpointRemoval::Stale;
    }
    if ipc.endpoint_waiters[index].is_some()
        || ipc.endpoint_sender_waiters[index]
            .iter()
            .any(Option::is_some)
        || ipc.irq_routes.iter().flatten().any(|route| *route == index)
    {
        return EndpointRemoval::Published;
    }
    ipc.endpoints[index] = None;
    let mut next = generation.wrapping_add(1);
    if next == 0 {
        next = 1;
    }
    ipc.endpoint_generations[index] = next;
    EndpointRemoval::Removed
}

// ── §3: the capability-only delegation owner ────────────────────────────────────────────────

/// The rank-2 half of a delegation: who is giving, who is receiving, and which cspaces those two
/// tasks own.
///
/// Snapshotted before capability rank 4 is acquired, and re-validated inside it. The old code read
/// `task_cnode(dest_tid)` in the middle of its capability work, which is a task-rank read taken
/// while deciding a capability-rank action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DelegationIdentity {
    pub(crate) source_tid: u64,
    pub(crate) source_cnode: CNodeId,
    pub(crate) dest_tid: u64,
    pub(crate) dest_cnode: CNodeId,
}

/// The exact rollback token for one delegation.
///
/// It names the destination cspace, the minted capability AND the object that capability carries.
/// The object is what makes a stale rollback safe: a slot can be recycled, so before revoking, the
/// release re-reads the slot and refuses unless it still holds this exact object. Without that, a
/// late rollback would revoke whatever unrelated capability had since taken the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DelegationGrant {
    pub(crate) identity: DelegationIdentity,
    pub(crate) source_cap: CapId,
    pub(crate) dest_cap: CapId,
    pub(crate) object: CapObject,
    /// True when a `DelegatedCapabilityLink` was recorded, so the release knows whether one has to
    /// come back out.
    pub(crate) linked: bool,
    /// True when the object carries a MemoryObject capability refcount that the CALLER incremented
    /// after this body returned, and must decrement on release. The body never touches it: memory
    /// is rank 6 and this body holds rank 4.
    pub(crate) owes_memory_refcount: bool,
}

/// Delegate one capability, under capability rank 4 and nothing else.
///
/// Everything is re-validated here rather than trusted from the snapshot, because the snapshot was
/// taken before this lock was held:
///
/// * both cspaces still exist, and are still the ones the identity names;
/// * the source capability is still present in the source cspace and still carries the same
///   OBJECT — including, for an endpoint or reply capability, the same generation — so a slot
///   recycled between snapshot and lock is refused rather than delegated;
/// * the requested rights are derivable from what the source actually holds. `Capability::derive`
///   is the single authority for that; this passes the request to it and mints exactly what comes
///   back, which is the intersection and never more.
///
/// No task, scheduler, IPC, VM or memory state is reachable from here. The two consequences are
/// stated in the token: the caller owns the MemoryObject refcount adjustment (rank 6), and the
/// caller owns any check of whether the underlying object is still LIVE (an endpoint generation
/// check is IPC rank 3) — which is why `expected_object` is an input, established by the caller
/// before it acquired rank 4.
pub(crate) fn delegate_capability_locked(
    capability: &mut CapabilitySubsystem,
    identity: &DelegationIdentity,
    source_cap: CapId,
    rights: CapRights,
    expected_object: CapObject,
    limits: CnodeGrowthLimits,
) -> Result<DelegationGrant, KernelError> {
    let source = capability
        .cnode_spaces
        .iter()
        .flatten()
        .find(|space| space.id == identity.source_cnode)
        .and_then(|space| kernel_ref(&space.cspace).get(source_cap))
        .ok_or(KernelError::InvalidCapability)?;
    if source.object != expected_object {
        // The slot was recycled between the caller's validation and this lock. Delegating now
        // would hand out rights over an object nobody asked about.
        crate::yarm_log!(
            "SPAWN_DELEGATE_STALE_SOURCE src_cnode={} src_cap={} expected={:?} found={:?}",
            identity.source_cnode.0,
            source_cap.0,
            expected_object,
            source.object
        );
        return Err(KernelError::StaleCapability);
    }
    let attenuated = source
        .derive(rights)
        .map_err(|_| KernelError::MissingRight)?;
    let dest_cap = mint_in_cnode_locked(
        capability,
        identity.dest_cnode,
        attenuated,
        limits.slot_capacity,
        limits.max_total_cnode_slots,
    )?;

    let mut linked = false;
    if identity.source_tid != identity.dest_tid {
        match record_delegation_link_locked(
            capability,
            identity.source_tid,
            source_cap,
            identity.dest_tid,
            dest_cap,
        ) {
            Ok(()) => linked = true,
            Err(err) => {
                // The link table is full. Give the slot back rather than leak it — and use the
                // narrow revoke, because this capability is one line old and has no descendants,
                // no transfer mapping and no notification.
                let revoked = revoke_fresh_cap_locked(capability, identity.dest_cnode, dest_cap);
                crate::yarm_log!(
                    "GRANT_CAP_LINK_FAIL_ROLLBACK src_tid={} src_cap={} dest_tid={} dest_cap={} err={:?} revoked={}",
                    identity.source_tid,
                    source_cap.0,
                    identity.dest_tid,
                    dest_cap.0,
                    err,
                    revoked
                );
                return Err(err);
            }
        }
    }
    Ok(DelegationGrant {
        identity: *identity,
        source_cap,
        dest_cap,
        object: attenuated.object,
        linked,
        owes_memory_refcount: matches!(
            attenuated.object,
            CapObject::MemoryObject { .. } | CapObject::DmaRegion { .. }
        ),
    })
}

/// `record_delegated_capability_link`'s body, under capability rank 4 and nothing else.
pub(crate) fn record_delegation_link_locked(
    capability: &mut CapabilitySubsystem,
    source_tid: u64,
    source_cap: CapId,
    dest_tid: u64,
    dest_cap: CapId,
) -> Result<(), KernelError> {
    let links = kernel_mut(&mut capability.delegated_capability_links);
    if links.iter().flatten().any(|link| {
        link.source_tid == source_tid
            && link.source_cap == source_cap
            && link.dest_tid == dest_tid
            && link.dest_cap == dest_cap
    }) {
        return Ok(());
    }
    if let Some(slot) = links.iter_mut().find(|slot| slot.is_none()) {
        *slot = Some(super::defs::DelegatedCapabilityLink {
            source_tid,
            source_cap,
            dest_tid,
            dest_cap,
        });
        Ok(())
    } else {
        Err(KernelError::CapabilityFull)
    }
}

/// Give one delegation back, under capability rank 4 and nothing else.
///
/// Refuses on a stale token rather than guessing: the destination slot must still hold the exact
/// object the grant recorded. A cspace slot that has been revoked and re-minted holds someone
/// else's capability, and revoking it because a token from a previous life names the same `CapId`
/// is precisely the bug this check exists to make impossible.
///
/// Returns whether the capability was actually removed. `false` means the token was stale, which
/// is a correct and inert outcome, not an error.
pub(crate) fn release_delegation_grant_locked(
    capability: &mut CapabilitySubsystem,
    grant: &DelegationGrant,
) -> bool {
    let current = capability
        .cnode_spaces
        .iter()
        .flatten()
        .find(|space| space.id == grant.identity.dest_cnode)
        .and_then(|space| kernel_ref(&space.cspace).get(grant.dest_cap));
    let Some(current) = current else {
        return false;
    };
    if current.object != grant.object {
        crate::yarm_log!(
            "SPAWN_DELEGATE_RELEASE_STALE dest_cnode={} dest_cap={} expected={:?} found={:?}",
            grant.identity.dest_cnode.0,
            grant.dest_cap.0,
            grant.object,
            current.object
        );
        return false;
    }
    if grant.linked {
        let links = kernel_mut(&mut capability.delegated_capability_links);
        for slot in links.iter_mut() {
            if slot.is_some_and(|link| {
                link.source_tid == grant.identity.source_tid
                    && link.source_cap == grant.source_cap
                    && link.dest_tid == grant.identity.dest_tid
                    && link.dest_cap == grant.dest_cap
            }) {
                *slot = None;
                break;
            }
        }
    }
    revoke_fresh_cap_locked(capability, grant.identity.dest_cnode, grant.dest_cap)
}
