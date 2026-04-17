// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

impl KernelState {
    pub(crate) fn ensure_cnode_space(&mut self, cnode: CNodeId) -> Result<(), KernelError> {
        let slot_capacity = crate::kernel::capabilities::MAX_CAPABILITIES_PER_CSPACE;
        self.ensure_cnode_space_with_slots(cnode, slot_capacity)
    }

    pub(crate) fn ensure_cnode_space_with_slots(
        &mut self,
        cnode: CNodeId,
        slot_capacity: usize,
    ) -> Result<(), KernelError> {
        let bounded_slot_capacity =
            slot_capacity.clamp(1, crate::kernel::capabilities::MAX_CAPABILITIES_PER_CSPACE);
        self.with_capability_state_mut(|capability| {
            if capability
                .cnode_spaces
                .iter()
                .flatten()
                .any(|space| space.id == cnode)
            {
                return Ok(());
            }
            if let Some(slot) = capability
                .cnode_spaces
                .iter_mut()
                .find(|slot| slot.is_none())
            {
                *slot = Some(CNodeSpace {
                    id: cnode,
                    slot_capacity: bounded_slot_capacity,
                    cspace: store_kernel_value(CapabilitySpace::with_slots(bounded_slot_capacity)),
                });
                Ok(())
            } else {
                Err(KernelError::TaskTableFull)
            }
        })
    }

    pub(crate) fn cnode_slot_capacity(&self, cnode: CNodeId) -> Option<usize> {
        self.with_capability_state(|capability| {
            capability
                .cnode_spaces
                .iter()
                .flatten()
                .find(|space| space.id == cnode)
                .map(|space| space.slot_capacity)
        })
    }

    pub(crate) fn mint_capability_for_current_context(
        &mut self,
        capability: Capability,
    ) -> Result<CapId, KernelError> {
        let cnode = self.current_task_cnode().ok_or(KernelError::TaskMissing)?;
        self.mint_capability_in_cnode(cnode, capability)
    }

    pub(crate) fn mint_capability_in_cnode(
        &mut self,
        cnode: CNodeId,
        capability: Capability,
    ) -> Result<CapId, KernelError> {
        self.ensure_cnode_space(cnode)?;
        let minted = self.with_capability_state_mut(|capability_state| {
            capability_state
                .cnode_spaces
                .iter_mut()
                .flatten()
                .find(|space| space.id == cnode)
                .map(|space| kernel_mut(&mut space.cspace))
                .ok_or(KernelError::TaskMissing)?
                .mint(capability)
                .map_err(|_| KernelError::CapabilityFull)
        })?;
        self.adjust_memory_object_cap_refcount(capability.object, 1);
        Ok(minted)
    }

    pub(crate) fn revoke_capability_in_cnode(
        &mut self,
        cnode: CNodeId,
        cap: CapId,
    ) -> Result<(), KernelError> {
        let source_capability = self.with_capability_state(|capability_state| {
            capability_state
                .cnode_spaces
                .iter()
                .flatten()
                .find(|space| space.id == cnode)
                .and_then(|space| kernel_ref(&space.cspace).get(cap))
        });
        let source_pid = self.tid_for_cnode(cnode).ok_or(KernelError::TaskMissing)?;
        let root = DelegatedCapRef {
            pid: source_pid,
            cap,
        };
        let descendants = self.collect_delegated_descendants(root);
        self.with_capability_state_mut(|capability_state| {
            capability_state
                .cnode_spaces
                .iter_mut()
                .flatten()
                .find(|space| space.id == cnode)
                .map(|space| kernel_mut(&mut space.cspace))
                .ok_or(KernelError::TaskMissing)?
                .revoke(cap)
                .map_err(|_| KernelError::InvalidCapability)
        })?;
        for delegated in descendants.into_iter().flatten() {
            self.revoke_capability_direct_in_process_cnode(delegated.pid, delegated.cap);
        }
        self.remove_delegation_links_for(root, descendants);
        self.revoke_active_transfer_mappings_for_cap(source_pid, cap);
        if let Some(capability) = source_capability {
            self.adjust_memory_object_cap_refcount(capability.object, -1);
            self.reclaim_memory_object_if_unreferenced(capability.object);
        }
        Ok(())
    }

    pub(crate) fn record_delegated_capability_link(
        &mut self,
        source_tid: u64,
        source_cap: CapId,
        dest_tid: u64,
        dest_cap: CapId,
    ) -> Result<(), KernelError> {
        self.with_capability_state_mut(|capability| {
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
                *slot = Some(DelegatedCapabilityLink {
                    source_tid,
                    source_cap,
                    dest_tid,
                    dest_cap,
                });
                Ok(())
            } else {
                Err(KernelError::CapabilityFull)
            }
        })
    }

    fn tid_for_cnode(&self, cnode: CNodeId) -> Option<u64> {
        self.with_capability_state(|capability| {
            capability
                .process_cnodes
                .iter()
                .flatten()
                .find(|record| record.cnode == cnode)
                .map(|record| record.pid)
        })
    }

    pub(crate) fn revoke_capability_direct_in_process_cnode(&mut self, pid: u64, cap: CapId) {
        let mut revoked_capability = None;
        if let Some(cnode) = self.process_cnode_for_pid(pid) {
            self.with_capability_state_mut(|capability_state| {
                if let Some(cspace) = capability_state
                    .cnode_spaces
                    .iter_mut()
                    .flatten()
                    .find(|space| space.id == cnode)
                    .map(|space| kernel_mut(&mut space.cspace))
                {
                    revoked_capability = cspace.get(cap);
                    let _ = cspace.revoke(cap);
                }
            });
        }
        self.revoke_active_transfer_mappings_for_cap(pid, cap);
        if let Some(capability) = revoked_capability {
            self.adjust_memory_object_cap_refcount(capability.object, -1);
            self.reclaim_memory_object_if_unreferenced(capability.object);
        }
    }

    fn revoke_active_transfer_mappings_for_cap(&mut self, owner_pid: u64, cap: CapId) {
        for idx in 0..MAX_TRANSFER_ENVELOPES {
            let mapping = self.with_ipc_state(|ipc| ipc.active_transfer_mappings[idx]);
            let Some(mapping) = mapping else {
                continue;
            };
            let mapping_pid = self
                .process_id(mapping.owner_tid.0)
                .unwrap_or(mapping.owner_tid.0);
            if mapping_pid != owner_pid || mapping.transfer_cap != cap {
                continue;
            }
            if let Some(asid) = self.task_asid(mapping.owner_tid.0) {
                let mut va = mapping.base.0 as usize;
                let end = va.saturating_add(mapping.len);
                while va < end {
                    let _ = self.unmap_user_page_in_asid(asid, VirtAddr(va as u64));
                    va = va.saturating_add(crate::kernel::vm::PAGE_SIZE);
                }
            }
            self.with_ipc_state_mut(|ipc| ipc.active_transfer_mappings[idx] = None);
            self.note_shared_mem_released(mapping.len);
            self.note_transfer_record_revoked();
            let _ = self.report_transfer_revoke_to_supervisor(
                owner_pid,
                cap.0,
                mapping.base.0,
                mapping.len as u64,
            );
            crate::yarm_log!(
                "YARM_TRANSFER_REVOKE owner_pid={} cap={} base=0x{:x} len={}",
                owner_pid,
                cap.0,
                mapping.base.0,
                mapping.len
            );
        }
    }
}
