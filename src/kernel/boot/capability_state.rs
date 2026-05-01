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

    pub fn grant_capability_task_to_task_with_rights(
        &mut self,
        source_tid: u64,
        source_cap: CapId,
        dest_tid: u64,
        rights: CapRights,
    ) -> Result<CapId, KernelError> {
        let capability = self.resolve_capability_for_task(source_tid, source_cap)?;
        let attenuated = capability
            .derive(rights)
            .map_err(|_| KernelError::MissingRight)?;
        let dest_cnode = self.task_cnode(dest_tid).ok_or(KernelError::TaskMissing)?;
        let delegated_cap = self.mint_capability_in_cnode(dest_cnode, attenuated)?;
        if source_tid != dest_tid {
            self.record_delegated_capability_link(source_tid, source_cap, dest_tid, delegated_cap)?;
        }
        Ok(delegated_cap)
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
