// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-SPAWN2 §2 — THE process-CNode transaction.
//!
//! # Why this exists
//!
//! Provisioning a process's capability space is two mutations that must either both happen or
//! neither: create the CNode space, and associate it with the PID that owns it. They lived as two
//! independent entries — [`KernelState::ensure_cnode_space_with_slots`] and
//! [`KernelState::set_process_cnode_for_pid`] — each taking its own rank-4 acquisition, with the
//! caller responsible for noticing that the second can fail after the first succeeded. Nobody
//! did. A `TaskTableFull` from the association left a CNode space provisioned for a process that
//! did not exist, and nothing ever removed it.
//!
//! It also blocked the live process-spawn route. U9-SPAWN1 SP-4 stopped because a process spawn
//! creates a NEW process CNode and there appeared to be no off-lock owner able to do it. That was
//! half wrong, and the correction is worth recording: `ensure_cnode_space_locked` already existed
//! as a rank-4-only owner with an off-lock entry (`SharedKernel::ensure_cnode_space_split`); the
//! earlier audit grepped for the BROAD wrapper's name and concluded from its absence that no
//! split path existed. `set_process_cnode_for_pid` genuinely had no rank-local sibling. So the
//! gap was one bounded extraction, not a subsystem.
//!
//! # The transaction
//!
//! One rank-4 acquisition performs both mutations. That is the whole mechanism, and it is what
//! makes the ordering guarantees true rather than merely intended:
//!
//! 1. **Identity.** The PID comes from the caller's spawn request and nowhere else. There is no
//!    ambient `current_tid()` fallback and nothing is fabricated: a caller that cannot name the
//!    process it is provisioning for cannot call this.
//! 2. **Provision** the CNode space at the capacity the class requires.
//! 3. **Associate** it with that exact PID.
//! 4. **Publish nothing** to the child in between. Because both steps happen under one
//!    acquisition, no other CPU can observe a space that exists but belongs to nobody, or an
//!    association naming a space that was never created — the intermediate state has no
//!    observer, rather than a short-lived one.
//! 5. **Compensate** through the owner that actually holds each half, and only for what THIS
//!    transaction created.
//!
//! # What the grant records, and why
//!
//! [`ProcessCNodeGrant`] carries two booleans because "undo this" is not the same as "remove the
//! CNode". A spawn into a process that already has a capability space — a second thread joining
//! its parent — provisions nothing and associates nothing, and its compensation must therefore
//! remove nothing. Recording what was created is what makes the compensation exact instead of
//! destructive, and it is what makes repetition inert: a grant that created nothing releases
//! nothing however many times it is replayed.
//!
//! # What this transaction does NOT own
//!
//! Capabilities minted INTO the space afterwards. Those belong to the spawn's resource ledger
//! (`crate::kernel::syscall::spawn_image_txn`), which revokes them through
//! `revoke_capability_in_cnode` — the owner that actually holds them. A CNode-space compensation
//! that also revoked caps would be a second capability-lifecycle policy, and the whole point of
//! this module is that there is one owner per resource.

use super::*;

/// The process a CNode space is being provisioned for.
///
/// Deliberately carries the PID explicitly. The broad reservation path used to derive it from
/// its own argument and the split path would have had to derive it from somewhere; making it a
/// field means both state it, and neither can quietly substitute the current task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessCNodeRequest {
    /// The owning process. For a process spawn this is the new task's own TID; for a thread
    /// joining an existing process it is that process's TID.
    pub(crate) pid: u64,
    /// The thread being provisioned into the process.
    pub(crate) tid: u64,
    /// Decides the slot capacity through the existing class policy.
    pub(crate) class: TaskClass,
}

/// What one process-CNode transaction actually created, and therefore what its compensation owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessCNodeGrant {
    pub(crate) cnode: CNodeId,
    /// This transaction created the CNode space. A pre-existing space is not ours to remove.
    pub(crate) created_space: bool,
    /// This transaction created the PID association. A pre-existing association is not ours to
    /// remove — and re-pointing one we did not create would silently move another process's
    /// capability space.
    pub(crate) created_association: bool,
}

impl ProcessCNodeGrant {
    /// Nothing was created, so nothing is owed. The shape a second thread joining an existing
    /// process produces.
    pub(crate) const fn nothing_created(cnode: CNodeId) -> Self {
        Self {
            cnode,
            created_space: false,
            created_association: false,
        }
    }

    /// Whether releasing this grant would do anything at all.
    pub(crate) const fn owns_anything(&self) -> bool {
        self.created_space || self.created_association
    }
}

/// The association currently recorded for `pid`, if any.
fn association_for(capability: &CapabilitySubsystem, pid: u64) -> Option<CNodeId> {
    capability
        .process_cnodes
        .iter()
        .flatten()
        .find(|record| record.pid == pid)
        .map(|record| record.cnode)
}

fn space_exists(capability: &CapabilitySubsystem, cnode: CNodeId) -> bool {
    capability
        .cnode_spaces
        .iter()
        .flatten()
        .any(|space| space.id == cnode)
}

/// THE rank-4 body: provision the space and associate it with the PID, or leave the subsystem
/// exactly as it was found.
///
/// Both mutations happen here, under the caller's single acquisition, so a failure of the second
/// undoes the first before anything outside this function can observe either.
pub(crate) fn provision_process_cnode_locked(
    capability: &mut CapabilitySubsystem,
    request: &ProcessCNodeRequest,
    bounded_slot_capacity: usize,
    max_total_cnode_slots: usize,
) -> Result<ProcessCNodeGrant, KernelError> {
    // The process keeps the CNode it already has. Only a process that has none gets one named
    // after it — the same rule the reservation path has always used, stated once.
    let existing_association = association_for(capability, request.pid);
    let cnode = existing_association.unwrap_or(CNodeId(request.pid));

    let space_was_present = space_exists(capability, cnode);
    KernelState::ensure_cnode_space_locked(
        capability,
        cnode,
        bounded_slot_capacity,
        max_total_cnode_slots,
    )?;
    let created_space = !space_was_present;

    if existing_association == Some(cnode) {
        crate::yarm_log!(
            "PROCESS_CNODE_TXN_OK pid={} tid={} cnode={} created_space={} created_assoc=0",
            request.pid,
            request.tid,
            cnode.0,
            u8::from(created_space)
        );
        return Ok(ProcessCNodeGrant {
            cnode,
            created_space,
            created_association: false,
        });
    }

    // Associate. This is the step that could fail after the space existed, which is the leak the
    // two independent entries left behind.
    let associated = if let Some(slot) = capability
        .process_cnodes
        .iter_mut()
        .find(|slot| slot.is_none())
    {
        *slot = Some(ProcessCNodeRecord {
            pid: request.pid,
            cnode,
        });
        true
    } else {
        false
    };
    if !associated {
        // Undo the space if THIS call created it, then fail. Nothing observed the space: it was
        // created and removed inside one acquisition.
        if created_space {
            remove_space_locked(capability, cnode);
        }
        crate::yarm_log!(
            "PROCESS_CNODE_TXN_FAIL pid={} tid={} cnode={} reason=process_cnode_table_full space_undone={}",
            request.pid,
            request.tid,
            cnode.0,
            u8::from(created_space)
        );
        return Err(KernelError::TaskTableFull);
    }

    crate::yarm_log!(
        "PROCESS_CNODE_TXN_OK pid={} tid={} cnode={} created_space={} created_assoc=1",
        request.pid,
        request.tid,
        cnode.0,
        u8::from(created_space)
    );
    Ok(ProcessCNodeGrant {
        cnode,
        created_space,
        created_association: true,
    })
}

/// Remove a CNode space by id. Inert when it is already gone.
fn remove_space_locked(capability: &mut CapabilitySubsystem, cnode: CNodeId) -> bool {
    if let Some(slot) = capability
        .cnode_spaces
        .iter_mut()
        .find(|slot| slot.as_ref().is_some_and(|space| space.id == cnode))
    {
        *slot = None;
        return true;
    }
    false
}

/// THE rank-4 compensation: release exactly what [`provision_process_cnode_locked`] created.
///
/// Inert by construction in every way that matters. A grant that created nothing removes
/// nothing. An association that has since been re-pointed at a different CNode belongs to
/// somebody else and is left alone. A space that is still associated with the PID is not removed,
/// because removing it would strip a live process of its capability space. Replaying the same
/// release finds each half already gone and does nothing.
pub(crate) fn release_process_cnode_grant_locked(
    capability: &mut CapabilitySubsystem,
    request: &ProcessCNodeRequest,
    grant: &ProcessCNodeGrant,
) {
    let mut removed_association = false;
    if grant.created_association
        && let Some(slot) = capability.process_cnodes.iter_mut().find(|slot| {
            slot.as_ref()
                .is_some_and(|record| record.pid == request.pid && record.cnode == grant.cnode)
        })
    {
        *slot = None;
        removed_association = true;
    }

    // Only remove the space once nothing points at it any more. If some other association still
    // names this CNode, it is in use and not ours to take.
    let still_referenced = capability
        .process_cnodes
        .iter()
        .flatten()
        .any(|record| record.cnode == grant.cnode);
    let removed_space =
        grant.created_space && !still_referenced && remove_space_locked(capability, grant.cnode);

    crate::yarm_log!(
        "PROCESS_CNODE_TXN_RELEASED pid={} tid={} cnode={} assoc={} space={}",
        request.pid,
        request.tid,
        grant.cnode.0,
        u8::from(removed_association),
        u8::from(removed_space)
    );
}

impl KernelState {
    /// The BROAD acquisition wrapper. The reservation path calls this rather than the two
    /// independent entries it used to, so both routes share one policy.
    pub(crate) fn provision_process_cnode(
        &mut self,
        request: &ProcessCNodeRequest,
    ) -> Result<ProcessCNodeGrant, KernelError> {
        let limits = self.runtime_capacity_config();
        let max_total_cnode_slots = limits.max_total_cnode_slots;
        let requested = Self::requested_cnode_slot_capacity_for_class(request.class, limits, None)?;
        let bounded = Self::normalize_requested_cnode_slots(requested, limits)?;
        self.with_capability_state_mut(|capability| {
            provision_process_cnode_locked(capability, request, bounded, max_total_cnode_slots)
        })
    }

    /// The BROAD compensation wrapper.
    pub(crate) fn release_process_cnode_grant(
        &mut self,
        request: &ProcessCNodeRequest,
        grant: &ProcessCNodeGrant,
    ) {
        if !grant.owns_anything() {
            return;
        }
        self.with_capability_state_mut(|capability| {
            release_process_cnode_grant_locked(capability, request, grant);
        });
    }
}
