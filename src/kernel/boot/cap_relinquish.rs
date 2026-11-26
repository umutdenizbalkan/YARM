// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! VFS-LIFETIME1 — NR 22 `CapRelinquish`: give up ONE capability slot the caller holds, without
//! touching any other holder of the same object.
//!
//! # Why a new operation
//!
//! A capability transfer is a delegation: the receiver gets a capability DERIVED from the
//! sender's (`grant_task_to_task_with_rights`), recorded as a delegation link
//! `(sender, cap) → (receiver, cap')`, and the sender keeps its own. A file grant
//! (`VFS_OP_FILE_GRANT_RO`) therefore leaves a chain initramfs → VFS → PM, and the only removal the
//! kernel offered was a REVOKE: NR 4 `TransferRelease` and `revoke_capability_in_cnode` remove the
//! slot AND, recursively, every delegated descendant. A relay that revoked its copy would destroy
//! the recipient's — so no relay ever released one, and every grant left three slots, two links and
//! one MemoryObject registry entry behind for the life of the kernel (VFS-LIFETIME1, F08).
//!
//! # The contract
//!
//! `arg0` names a capability in the caller's own process CNode. It must be a `MemoryObject`.
//!
//! * **What is released:** that one slot, and the object reference it carried
//!   (`cap_refcount - 1`; the object is reclaimed when no capability, mapping or pin remains —
//!   for an initramfs slice that only removes the registry entry, the initrd backing is borrowed).
//! * **What survives:** every other holder. A delegated child of the slot keeps its capability,
//!   its rights and its mappings. Its link is RE-PARENTED to the slot's own delegation parent when
//!   the slot has one, so every ancestor keeps exactly the revocation authority it had; when the slot
//!   is a root, the children become roots — the owner gives up authority it alone held.
//! * **Refusals — each raised before any mutation, each leaving everything as it was:**
//!   * `InvalidCapability`: the id does not name a live slot (stale generation, already released).
//!     A CapId carries its slot's generation and every removal bumps it, so a retried or late
//!     release can never remove a NEWER capability that reused the slot. Callers treat this as
//!     "already done".
//!   * `WrongObject`: not a `MemoryObject`; or the slot has derived children in its own CNode; or
//!     an active transfer mapping is registered on it (a mapped transfer is released by NR 4).
//!   * `WouldBlock`: a live transfer envelope still names this slot as its source (a queued
//!     message whose receiver will derive its capability from it), or the link table moved between
//!     the plan and the commit. Retry later.
//!
//! # Order
//!
//! ```text
//! P  rank 2/4 reads: caller, CNode, pid, the capability; rank 3 reads: envelopes, mappings;
//!    rank 4 snapshot of the link table, then rank 2 tid→pid resolution — the plan
//! C  rank 4, ONE acquisition: re-check the slot and every planned link by content, clear the
//!    slot (only if it is a leaf in its CNode), rewrite the links
//! M  rank 6: cap_refcount - 1, reclaim if unreferenced
//! ```
//!
//! No domain lock is held while another is taken, so the broad (hosted) adapter and the split
//! adapter run the same steps in the same order.

use super::*;

/// Why NR 22 refused. Every variant is raised before the commit mutates anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelinquishRefusal {
    NoCaller,
    Unresolvable,
    NotMemoryObject,
    HasLocalChildren,
    Mapped,
    EnvelopePending,
    Moved,
    OutOfMemory,
}

impl RelinquishRefusal {
    pub(crate) fn syscall_error(self) -> crate::kernel::syscall::SyscallError {
        use crate::kernel::syscall::SyscallError;
        match self {
            Self::NoCaller => SyscallError::Internal,
            Self::Unresolvable => SyscallError::InvalidCapability,
            Self::NotMemoryObject | Self::HasLocalChildren | Self::Mapped => {
                SyscallError::WrongObject
            }
            Self::EnvelopePending | Self::Moved => SyscallError::WouldBlock,
            Self::OutOfMemory => SyscallError::QueueFull,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NoCaller => "no_caller",
            Self::Unresolvable => "unresolvable",
            Self::NotMemoryObject => "not_memory_object",
            Self::HasLocalChildren => "local_children",
            Self::Mapped => "mapped",
            Self::EnvelopePending => "envelope_pending",
            Self::Moved => "moved",
            Self::OutOfMemory => "out_of_memory",
        }
    }
}

/// The committed result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RelinquishOutcome {
    pub(crate) pid: u64,
    pub(crate) object: CapObject,
    /// Delegated children whose link now names the slot's parent (or no parent).
    pub(crate) reparented: usize,
    /// The slot's own delegation parent, if it had one.
    pub(crate) parent: Option<(u64, CapId)>,
    /// The object left the registry with this release.
    pub(crate) reclaimed: bool,
}

/// The link edits, planned from a snapshot and re-checked by content at the commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelinquishLinkPlan {
    /// The link that names this slot as its DEST: `(index, link)`.
    pub(crate) parent: Option<(usize, DelegatedCapabilityLink)>,
    /// Every link that names this slot as its SOURCE.
    pub(crate) children: alloc::vec::Vec<(usize, DelegatedCapabilityLink)>,
}

/// THE pure planner: which links name `(pid, cap)` as source or dest. `pid_of` resolves a link's
/// TID to its process exactly as the revoke closure does (an unknown TID stands for itself).
pub(crate) fn plan_relinquish_links(
    links: &[Option<DelegatedCapabilityLink>],
    pid: u64,
    cap: CapId,
    pid_of: impl Fn(u64) -> u64,
) -> Result<RelinquishLinkPlan, RelinquishRefusal> {
    let mut children = alloc::vec::Vec::new();
    children
        .try_reserve(links.iter().flatten().count())
        .map_err(|_| RelinquishRefusal::OutOfMemory)?;
    let mut parent = None;
    for (index, link) in links.iter().enumerate() {
        let Some(link) = *link else { continue };
        if pid_of(link.source_tid) == pid && link.source_cap == cap {
            children.push((index, link));
        }
        if pid_of(link.dest_tid) == pid && link.dest_cap == cap {
            // A slot is materialized once, from one source, so it has at most one parent link.
            if parent.is_some() {
                return Err(RelinquishRefusal::Moved);
            }
            parent = Some((index, link));
        }
    }
    Ok(RelinquishLinkPlan { parent, children })
}

/// THE rank-4 commit body, shared by both adapters: everything re-checked by content, then the
/// slot cleared and the links rewritten in one acquisition.
pub(crate) fn commit_relinquish_locked(
    capability: &mut CapabilitySubsystem,
    cnode: CNodeId,
    cap: CapId,
    expected: Capability,
    plan: &RelinquishLinkPlan,
) -> Result<(), RelinquishRefusal> {
    {
        let links = kernel_ref(&capability.delegated_capability_links);
        let planned = plan.parent.iter().chain(plan.children.iter());
        for (index, link) in planned {
            if links.get(*index).copied().flatten() != Some(*link) {
                return Err(RelinquishRefusal::Moved);
            }
        }
    }
    {
        let space = capability
            .cnode_spaces
            .iter_mut()
            .flatten()
            .find(|space| space.id == cnode)
            .ok_or(RelinquishRefusal::Unresolvable)?;
        let cspace = kernel_mut(&mut space.cspace);
        if cspace.get(cap) != Some(expected) {
            return Err(RelinquishRefusal::Unresolvable);
        }
        // Atomic: refuses (mutating nothing) when the slot has derived children in this CNode.
        match cspace.delete_if_leaf(cap) {
            Ok(true) => {}
            Ok(false) => return Err(RelinquishRefusal::HasLocalChildren),
            Err(_) => return Err(RelinquishRefusal::Unresolvable),
        }
    }
    let links = kernel_mut(&mut capability.delegated_capability_links);
    match plan.parent {
        Some((parent_index, parent_link)) => {
            for (index, _) in &plan.children {
                if let Some(link) = links[*index].as_mut() {
                    link.source_tid = parent_link.source_tid;
                    link.source_cap = parent_link.source_cap;
                }
            }
            links[parent_index] = None;
        }
        None => {
            for (index, _) in &plan.children {
                links[*index] = None;
            }
        }
    }
    Ok(())
}

/// Every domain step the transaction takes. One method, one acquisition.
pub(crate) trait RelinquishOwners {
    /// rank 2 then rank 4: the caller's process CNode and pid.
    fn caller_cnode_and_pid(&self, tid: u64) -> Option<(CNodeId, u64)>;
    /// rank 4: the capability in that CNode.
    fn resolve(&self, cnode: CNodeId, cap: CapId) -> Option<Capability>;
    /// rank 3: the TIDs of every live transfer envelope whose source cap id is `cap`, and of every
    /// active transfer mapping registered on `cap`. At most 8 each; `true` = more than 8.
    fn envelope_sources(&self, cap: CapId) -> ([u64; 8], usize, bool);
    fn mapping_owners(&self, cap: CapId) -> ([u64; 8], usize, bool);
    /// rank 2: a TID's process.
    fn pid_of(&self, tid: u64) -> u64;
    /// rank 4: a snapshot of the delegation-link table.
    fn links_snapshot(&self) -> alloc::boxed::Box<[Option<DelegatedCapabilityLink>]>;
    /// rank 4: [`commit_relinquish_locked`].
    fn commit(
        &self,
        cnode: CNodeId,
        cap: CapId,
        expected: Capability,
        plan: &RelinquishLinkPlan,
    ) -> Result<(), RelinquishRefusal>;
    /// rank 6: one capability reference released; `true` if the object left the registry.
    fn release_reference(&self, object: CapObject) -> bool;
}

fn names_pid<O: RelinquishOwners + ?Sized>(
    owners: &O,
    found: ([u64; 8], usize, bool),
    pid: u64,
) -> bool {
    let (tids, count, overflow) = found;
    overflow || tids[..count].iter().any(|tid| owners.pid_of(*tid) == pid)
}

/// THE NR 22 transaction.
pub(crate) fn run_relinquish<O: RelinquishOwners + ?Sized>(
    owners: &O,
    tid: u64,
    cap: CapId,
) -> Result<RelinquishOutcome, RelinquishRefusal> {
    // ── P: the plan. Reads only. ──
    let (cnode, pid) = owners
        .caller_cnode_and_pid(tid)
        .ok_or(RelinquishRefusal::NoCaller)?;
    let expected = owners
        .resolve(cnode, cap)
        .ok_or(RelinquishRefusal::Unresolvable)?;
    if !matches!(expected.object, CapObject::MemoryObject { .. }) {
        return Err(RelinquishRefusal::NotMemoryObject);
    }
    if names_pid(owners, owners.mapping_owners(cap), pid) {
        return Err(RelinquishRefusal::Mapped);
    }
    if names_pid(owners, owners.envelope_sources(cap), pid) {
        return Err(RelinquishRefusal::EnvelopePending);
    }
    let links = owners.links_snapshot();
    let plan = plan_relinquish_links(&links, pid, cap, |t| owners.pid_of(t))?;
    // ── C: the commit. The first and only capability mutation. ──
    owners.commit(cnode, cap, expected, &plan)?;
    // ── M: the reference the slot carried. ──
    let reclaimed = owners.release_reference(expected.object);
    Ok(RelinquishOutcome {
        pid,
        object: expected.object,
        reparented: plan.children.len(),
        parent: plan
            .parent
            .map(|(_, link)| (owners.pid_of(link.source_tid), link.source_cap)),
        reclaimed,
    })
}

fn release_reference_locked(memory: &mut MemorySubsystem, object: CapObject) -> bool {
    KernelState::adjust_memory_object_cap_refcount_locked(memory, object, -1);
    KernelState::reclaim_memory_object_if_unreferenced_locked(memory, object);
    let CapObject::MemoryObject { id } = object else {
        return false;
    };
    !memory
        .memory_objects
        .iter()
        .flatten()
        .any(|entry| entry.id == id)
}

fn collect_tids(ipc: &IpcSubsystem, cap: CapId, envelopes: bool) -> ([u64; 8], usize, bool) {
    let mut tids = [0u64; 8];
    let mut count = 0usize;
    let mut overflow = false;
    let mut note = |tid: u64| {
        if count < tids.len() {
            tids[count] = tid;
            count += 1;
        } else {
            overflow = true;
        }
    };
    if envelopes {
        for envelope in ipc.transfer_envelopes.iter().flatten() {
            if envelope.source_cap == cap {
                note(envelope.source_tid.0);
            }
        }
    } else {
        for mapping in ipc.active_transfer_mappings.iter().flatten() {
            if mapping.transfer_cap == cap {
                note(mapping.owner_tid.0);
            }
        }
    }
    (tids, count, overflow)
}

/// The broad adapter (global lock held; hosted builds and the broad dispatcher).
pub(crate) struct BroadRelinquishOwners<'a> {
    pub(crate) kernel: core::cell::RefCell<&'a mut KernelState>,
}

impl RelinquishOwners for BroadRelinquishOwners<'_> {
    fn caller_cnode_and_pid(&self, tid: u64) -> Option<(CNodeId, u64)> {
        let kernel = self.kernel.borrow();
        let pid = kernel.process_id(tid).unwrap_or(tid);
        Some((kernel.task_cnode(tid)?, pid))
    }
    fn resolve(&self, cnode: CNodeId, cap: CapId) -> Option<Capability> {
        self.kernel.borrow().with_capability_state(|capability| {
            capability
                .cnode_spaces
                .iter()
                .flatten()
                .find(|space| space.id == cnode)
                .and_then(|space| kernel_ref(&space.cspace).get(cap))
        })
    }
    fn envelope_sources(&self, cap: CapId) -> ([u64; 8], usize, bool) {
        self.kernel
            .borrow()
            .with_ipc_state(|ipc| collect_tids(ipc, cap, true))
    }
    fn mapping_owners(&self, cap: CapId) -> ([u64; 8], usize, bool) {
        self.kernel
            .borrow()
            .with_ipc_state(|ipc| collect_tids(ipc, cap, false))
    }
    fn pid_of(&self, tid: u64) -> u64 {
        self.kernel.borrow().process_id(tid).unwrap_or(tid)
    }
    fn links_snapshot(&self) -> alloc::boxed::Box<[Option<DelegatedCapabilityLink>]> {
        self.kernel.borrow().with_capability_state(|capability| {
            kernel_ref(&capability.delegated_capability_links)
                .to_vec()
                .into_boxed_slice()
        })
    }
    fn commit(
        &self,
        cnode: CNodeId,
        cap: CapId,
        expected: Capability,
        plan: &RelinquishLinkPlan,
    ) -> Result<(), RelinquishRefusal> {
        self.kernel
            .borrow_mut()
            .with_capability_state_mut(|capability| {
                commit_relinquish_locked(capability, cnode, cap, expected, plan)
            })
    }
    fn release_reference(&self, object: CapObject) -> bool {
        self.kernel
            .borrow_mut()
            .with_memory_state_mut(|memory| release_reference_locked(memory, object))
    }
}

/// The split adapter: each method one domain acquisition through the `SharedKernel` seams.
#[cfg_attr(all(feature = "hosted-dev", not(test)), allow(dead_code))]
pub(crate) struct SplitRelinquishOwners<'a> {
    pub(crate) shared: &'a crate::runtime::SharedKernel,
}

impl RelinquishOwners for SplitRelinquishOwners<'_> {
    fn caller_cnode_and_pid(&self, tid: u64) -> Option<(CNodeId, u64)> {
        let cnode = self.shared.task_cnode_split(tid)?;
        let pid = self.shared.pid_for_cnode_split(cnode)?;
        Some((cnode, pid))
    }
    fn resolve(&self, cnode: CNodeId, cap: CapId) -> Option<Capability> {
        self.shared.resolved_capability_split(cnode, cap)
    }
    fn envelope_sources(&self, cap: CapId) -> ([u64; 8], usize, bool) {
        self.shared
            .with_ipc_split_mut(|ipc| collect_tids(ipc, cap, true))
    }
    fn mapping_owners(&self, cap: CapId) -> ([u64; 8], usize, bool) {
        self.shared
            .with_ipc_split_mut(|ipc| collect_tids(ipc, cap, false))
    }
    fn pid_of(&self, tid: u64) -> u64 {
        self.shared.process_id_split_read(tid).unwrap_or(tid)
    }
    fn links_snapshot(&self) -> alloc::boxed::Box<[Option<DelegatedCapabilityLink>]> {
        self.shared.with_capability_state_split_mut(|capability| {
            kernel_ref(&capability.delegated_capability_links)
                .to_vec()
                .into_boxed_slice()
        })
    }
    fn commit(
        &self,
        cnode: CNodeId,
        cap: CapId,
        expected: Capability,
        plan: &RelinquishLinkPlan,
    ) -> Result<(), RelinquishRefusal> {
        self.shared.with_capability_state_split_mut(|capability| {
            commit_relinquish_locked(capability, cnode, cap, expected, plan)
        })
    }
    fn release_reference(&self, object: CapObject) -> bool {
        self.shared
            .with_memory_split_mut(|memory| release_reference_locked(memory, object))
    }
}

/// The one log line every committed release prints (both routes).
pub(crate) fn log_relinquish(
    tid: u64,
    cap: CapId,
    result: &Result<RelinquishOutcome, RelinquishRefusal>,
) {
    match result {
        Ok(outcome) => {
            let id = match outcome.object {
                CapObject::MemoryObject { id } => id,
                _ => 0,
            };
            let (parent_pid, parent_cap) = outcome.parent.map(|(p, c)| (p, c.0)).unwrap_or((0, 0));
            crate::yarm_log!(
                "CAP_RELINQUISH_OK tid={} pid={} cap={} mo_id={} reparented={} parent_pid={} parent_cap={} reclaimed={}",
                tid,
                outcome.pid,
                cap.0,
                id,
                outcome.reparented,
                parent_pid,
                parent_cap,
                u8::from(outcome.reclaimed)
            );
        }
        Err(refusal) => {
            crate::yarm_log!(
                "CAP_RELINQUISH_REFUSED tid={} cap={} reason={}",
                tid,
                cap.0,
                refusal.label()
            );
        }
    }
}
