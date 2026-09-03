// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;
use alloc::vec::Vec;

impl KernelState {
    pub fn current_task_capability(&self, cap: CapId) -> Option<Capability> {
        let cnode = self.current_task_cnode()?;
        self.capability_for_cnode(cnode, cap)
    }

    pub fn task_capability(&self, tid: u64, cap: CapId) -> Option<Capability> {
        let cnode = self.task_cnode(tid)?;
        self.capability_for_cnode(cnode, cap)
    }

    pub(crate) fn resolve_capability_for_task(
        &self,
        tid: u64,
        cap: CapId,
    ) -> Result<Capability, KernelError> {
        self.task_capability(tid, cap)
            .ok_or(KernelError::InvalidCapability)
    }

    pub fn current_task_capability_has_right(&self, cap: CapId, right: CapRights) -> bool {
        self.current_task_capability(cap)
            .map(|capability| capability.has_right(right))
            .unwrap_or(false)
    }

    #[cfg(test)]
    pub(crate) fn grant_capability_task_to_task(
        &mut self,
        source_tid: u64,
        source_cap: CapId,
        dest_tid: u64,
    ) -> Result<CapId, KernelError> {
        let capability = self.resolve_capability_for_task(source_tid, source_cap)?;
        let dest_cnode = self.task_cnode(dest_tid).ok_or(KernelError::TaskMissing)?;
        let delegated_cap = self.mint_capability_in_cnode(dest_cnode, capability)?;
        if source_tid != dest_tid {
            self.record_delegated_capability_link(source_tid, source_cap, dest_tid, delegated_cap)?;
        }
        Ok(delegated_cap)
    }

    /// Delegate a capability from one task to another, attenuated to `rights`.
    ///
    /// U9-SPAWN-IC1 split this into the three pieces its ranks always wanted:
    ///
    /// 1. **a rank-2 identity snapshot** — both TIDs' CNodes, resolved here, before capability
    ///    rank 4 is acquired. The old body read `task_cnode(dest_tid)` in the middle of its
    ///    capability work;
    /// 2. **a rank-3 liveness check** — `resolve_capability_for_task` reaches
    ///    `capability_object_live`, which reads IPC generations. It stays here, ahead of rank 4, so
    ///    the order is 2 → 3 → 4 and never 4 → 3;
    /// 3. **one capability-only rank-4 body**, [`spawn_ipc_cap_txn::delegate_capability_locked`],
    ///    which re-validates both cspaces and the source object under the lock and mints exactly
    ///    the rights intersection.
    ///
    /// The MemoryObject capability refcount the old body incremented inside
    /// `mint_capability_in_cnode` is now applied HERE, after rank 4 releases, because it is memory
    /// rank 6. The token says whether it is owed, so the two cannot drift.
    pub fn grant_capability_task_to_task_with_rights(
        &mut self,
        source_tid: u64,
        source_cap: CapId,
        dest_tid: u64,
        rights: CapRights,
    ) -> Result<CapId, KernelError> {
        let grant = self.delegate_capability(source_tid, source_cap, dest_tid, rights)?;
        Ok(grant.dest_cap)
    }

    /// The broad acquisition wrapper for the delegation body, returning the exact rollback token.
    ///
    /// Callers that only need the resulting `CapId` use
    /// [`Self::grant_capability_task_to_task_with_rights`]; callers that may have to undo the
    /// delegation take the token.
    pub(crate) fn delegate_capability(
        &mut self,
        source_tid: u64,
        source_cap: CapId,
        dest_tid: u64,
        rights: CapRights,
    ) -> Result<super::spawn_ipc_cap_txn::DelegationGrant, KernelError> {
        // ── 2 → 3: identities, then the liveness of the object being delegated. ─────────
        let capability = self.resolve_capability_for_task(source_tid, source_cap)?;
        let source_cnode = self
            .task_cnode(source_tid)
            .ok_or(KernelError::TaskMissing)?;
        let dest_cnode = self.task_cnode(dest_tid).ok_or(KernelError::TaskMissing)?;
        let identity = super::spawn_ipc_cap_txn::DelegationIdentity {
            source_tid,
            source_cnode,
            dest_tid,
            dest_cnode,
        };
        // ── 4: the capability-only body. Its growth limits come from the capacity config,
        //        read and normalized here so the body reads no config domain of its own.
        let limits = self.runtime_capacity_config();
        let cnode_limits = super::spawn_ipc_cap_txn::CnodeGrowthLimits {
            slot_capacity: Self::normalize_requested_cnode_slots(
                crate::kernel::capabilities::MAX_CAPABILITIES_PER_CSPACE,
                limits,
            )?,
            max_total_cnode_slots: limits.max_total_cnode_slots,
        };
        let grant = self.with_capability_state_mut(|capability_state| {
            super::spawn_ipc_cap_txn::delegate_capability_locked(
                capability_state,
                &identity,
                source_cap,
                rights,
                capability.object,
                cnode_limits,
            )
        })?;
        // ── 6: the memory-object refcount, once rank 4 is released. ────────────────────
        if grant.owes_memory_refcount {
            self.adjust_memory_object_cap_refcount(grant.object, 1);
        }
        Ok(grant)
    }

    /// Undo one delegation, in the exact reverse of [`Self::delegate_capability`].
    ///
    /// Rank 4 removes the delegation link and the capability — refusing if the destination slot no
    /// longer holds the exact object the token names, so a stale token can never revoke a recycled
    /// capability belonging to someone else. Only if that removal actually happened does the
    /// memory refcount come back down, which is what keeps the two symmetric.
    pub(crate) fn release_delegation(
        &mut self,
        grant: &super::spawn_ipc_cap_txn::DelegationGrant,
    ) -> bool {
        let released = self.with_capability_state_mut(|capability_state| {
            super::spawn_ipc_cap_txn::release_delegation_grant_locked(capability_state, grant)
        });
        if released && grant.owes_memory_refcount {
            self.adjust_memory_object_cap_refcount(grant.object, -1);
            self.reclaim_memory_object_if_unreferenced(grant.object);
        }
        crate::yarm_log!(
            "SPAWN_DELEGATE_RELEASED dest_cnode={} dest_cap={} released={}",
            grant.identity.dest_cnode.0,
            grant.dest_cap.0,
            u8::from(released)
        );
        released
    }

    pub fn capability_for_cnode(&self, cnode: CNodeId, cap: CapId) -> Option<Capability> {
        let capability = self.capability_for_cnode_local(cnode, cap)?;
        self.capability_object_live(capability.object)?;
        Some(capability)
    }

    pub(crate) fn capability_for_cnode_local(
        &self,
        cnode: CNodeId,
        cap: CapId,
    ) -> Option<Capability> {
        self.with_capability_state(|capability| {
            capability
                .cnode_spaces
                .iter()
                .flatten()
                .find(|space| space.id == cnode)
                .and_then(|space| kernel_ref(&space.cspace).get(cap))
        })
    }

    pub fn cnode_capability_has_right(&self, cnode: CNodeId, cap: CapId, right: CapRights) -> bool {
        self.capability_for_cnode(cnode, cap)
            .map(|capability| capability.has_right(right))
            .unwrap_or(false)
    }

    pub(crate) fn snapshot_live_capabilities_for_task(
        &self,
        tid: u64,
    ) -> Result<Vec<(CapId, Capability)>, KernelError> {
        let cnode = self.task_cnode(tid).ok_or(KernelError::TaskMissing)?;
        let local_ids = self.with_capability_state(|capability_state| {
            capability_state
                .cnode_spaces
                .iter()
                .flatten()
                .find(|space| space.id == cnode)
                .map(|space| kernel_ref(&space.cspace).live_cap_ids().collect::<Vec<_>>())
        });
        let Some(local_ids) = local_ids else {
            return Err(KernelError::TaskMissing);
        };
        let mut snapshot = Vec::new();
        for cap in local_ids {
            if let Some(capability) = self.capability_for_cnode(cnode, cap) {
                snapshot.push((cap, capability));
            }
        }
        Ok(snapshot)
    }
}
