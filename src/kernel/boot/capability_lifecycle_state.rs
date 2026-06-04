// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

impl KernelState {
    /// Runtime bounds for a requested CNode slot capacity:
    /// - must be non-zero
    /// - must be within per-CNode policy (`max_capability_slots`)
    /// - must fit within `CapId` index encoding
    ///
    /// Global pool accounting (`max_total_cnode_slots`) is enforced by the
    /// caller before committing create/resize.
    fn normalize_requested_cnode_slots(
        slot_capacity: usize,
        limits: RuntimeCapacityConfig,
    ) -> Result<usize, KernelError> {
        if slot_capacity == 0 {
            return Err(KernelError::WrongObject);
        }
        let max_slots_per_cnode = limits.max_capability_slots;
        if slot_capacity > max_slots_per_cnode {
            return Err(KernelError::CapabilityFull);
        }
        if slot_capacity > (CapId::INDEX_MASK as usize).saturating_add(1) {
            return Err(KernelError::WrongObject);
        }
        Ok(slot_capacity)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn control_plane_set_process_cnode_slots(
        &mut self,
        requester_tid: u64,
        target_pid: u64,
        slot_capacity: usize,
    ) -> Result<(), KernelError> {
        let requester_class = self
            .task_class(requester_tid)
            .ok_or(KernelError::TaskMissing)?;
        let requester_pid = self.process_id(requester_tid).unwrap_or(requester_tid);
        let requester_is_system_server = requester_class == TaskClass::SystemServer;
        if !requester_is_system_server && requester_pid != target_pid {
            return Err(KernelError::MissingRight);
        }

        if let Some(existing_cnode) = self.process_cnode_for_pid(target_pid) {
            if requester_is_system_server {
                self.resize_cnode_slots(existing_cnode, slot_capacity)
            } else {
                self.resize_process_cnode_slots(target_pid, slot_capacity)
            }
        } else {
            let target_cnode = CNodeId(target_pid);
            self.ensure_cnode_space_with_slots(target_cnode, slot_capacity)?;
            self.set_process_cnode_for_pid(target_pid, target_cnode)
        }
    }

    /// Stage 5B plan-first variant of `control_plane_set_process_cnode_slots`.
    ///
    /// Uses `plan.requester_class` and `plan.requester_pid` (snapshotted from the
    /// task domain, rank 2) instead of re-reading task state inside the capability
    /// mutation (rank 4). This eliminates the task→capability lock re-entry that
    /// `resize_process_cnode_slots` would otherwise perform.
    ///
    /// Lock-domain flow: caller already holds snapshot (no lock) → this function
    /// only acquires capability lock (rank 4) via `process_cnode_for_pid`,
    /// `resize_cnode_slots`, `ensure_cnode_space_with_slots`, and
    /// `set_process_cnode_for_pid`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn control_plane_set_process_cnode_slots_planned(
        &mut self,
        plan: &ControlPlaneCnodePlan,
        target_pid: u64,
        slot_capacity: usize,
    ) -> Result<(), KernelError> {
        let requester_is_system_server = plan.requester_class == TaskClass::SystemServer;
        if !requester_is_system_server && plan.requester_pid != target_pid {
            return Err(KernelError::MissingRight);
        }

        if let Some(existing_cnode) = self.process_cnode_for_pid(target_pid) {
            if requester_is_system_server {
                self.resize_cnode_slots(existing_cnode, slot_capacity)
            } else {
                // Non-system-server can only resize its own cnode (requester_pid == target_pid).
                // Use plan.requester_class for the class guard — it IS the target's class here.
                match plan.requester_class {
                    TaskClass::Driver | TaskClass::SystemServer => {}
                    TaskClass::App => return Err(KernelError::MissingRight),
                }
                self.resize_cnode_slots(existing_cnode, slot_capacity)
            }
        } else {
            let target_cnode = CNodeId(target_pid);
            self.ensure_cnode_space_with_slots(target_cnode, slot_capacity)?;
            self.set_process_cnode_for_pid(target_pid, target_cnode)
        }
    }

    pub(crate) fn ensure_cnode_space(&mut self, cnode: CNodeId) -> Result<(), KernelError> {
        let slot_capacity = crate::kernel::capabilities::MAX_CAPABILITIES_PER_CSPACE;
        self.ensure_cnode_space_with_slots(cnode, slot_capacity)
    }

    pub(crate) fn ensure_cnode_space_with_slots(
        &mut self,
        cnode: CNodeId,
        slot_capacity: usize,
    ) -> Result<(), KernelError> {
        let limits = self.runtime_capacity_config();
        let max_total_cnode_slots = limits.max_total_cnode_slots;
        let bounded_slot_capacity = Self::normalize_requested_cnode_slots(slot_capacity, limits)?;
        self.with_capability_state_mut(|capability| {
            if capability
                .cnode_spaces
                .iter()
                .flatten()
                .any(|space| space.id == cnode)
            {
                return Ok(());
            }
            let reserved_slots: usize = capability
                .cnode_spaces
                .iter()
                .flatten()
                .map(|space| space.slot_capacity)
                .sum();
            if reserved_slots.saturating_add(bounded_slot_capacity) > max_total_cnode_slots {
                return Err(KernelError::CapabilityFull);
            }

            if let Some(slot) = capability
                .cnode_spaces
                .iter_mut()
                .find(|slot| slot.is_none())
            {
                let cspace = CapabilitySpace::try_with_slots(bounded_slot_capacity)
                    .map_err(|_| KernelError::CapabilityFull)?;
                *slot = Some(CNodeSpace {
                    id: cnode,
                    slot_capacity: bounded_slot_capacity,
                    cspace: store_kernel_value(cspace),
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

    /// Returns the number of occupied (non-empty) slots in the given CNode.
    /// Used for diagnostics and test assertions.
    pub(crate) fn cnode_occupied_slots(&self, cnode: CNodeId) -> Option<usize> {
        self.with_capability_state(|capability| {
            capability
                .cnode_spaces
                .iter()
                .flatten()
                .find(|space| space.id == cnode)
                .map(|space| kernel_ref(&space.cspace).occupied_slots())
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn resize_process_cnode_slots(
        &mut self,
        process_pid: u64,
        slot_capacity: usize,
    ) -> Result<(), KernelError> {
        let class = self
            .task_class(process_pid)
            .ok_or(KernelError::TaskMissing)?;
        match class {
            TaskClass::Driver | TaskClass::SystemServer => {}
            TaskClass::App => return Err(KernelError::MissingRight),
        }
        let cnode = self
            .process_cnode_for_pid(process_pid)
            .ok_or(KernelError::TaskMissing)?;
        self.resize_cnode_slots(cnode, slot_capacity)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn resize_cnode_slots(
        &mut self,
        cnode: CNodeId,
        slot_capacity: usize,
    ) -> Result<(), KernelError> {
        let limits = self.runtime_capacity_config();
        let max_total_cnode_slots = limits.max_total_cnode_slots;
        let bounded_slot_capacity = Self::normalize_requested_cnode_slots(slot_capacity, limits)?;
        self.with_capability_state_mut(|capability| {
            let reserved_other_slots: usize = capability
                .cnode_spaces
                .iter()
                .flatten()
                .filter(|space| space.id != cnode)
                .map(|space| space.slot_capacity)
                .sum();
            if reserved_other_slots.saturating_add(bounded_slot_capacity) > max_total_cnode_slots {
                return Err(KernelError::CapabilityFull);
            }
            let space = capability
                .cnode_spaces
                .iter_mut()
                .flatten()
                .find(|space| space.id == cnode)
                .ok_or(KernelError::TaskMissing)?;
            kernel_mut(&mut space.cspace)
                .resize_slots(bounded_slot_capacity)
                .map_err(|err| match err {
                    CapabilityDeriveError::SpaceFull => KernelError::CapabilityFull,
                    CapabilityDeriveError::AllocFailed => KernelError::CapabilityFull,
                    CapabilityDeriveError::InvalidSlot => KernelError::WrongObject,
                    _ => KernelError::WrongObject,
                })?;
            space.slot_capacity = bounded_slot_capacity;
            Ok(())
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

    /// Narrow, no-alloc helper for recycling a one-shot Reply cap slot.
    ///
    /// Removes exactly one cap slot from `cnode` if and only if that slot
    /// currently contains `expected_object` referenced by `cap` (generation
    /// and index both match the CapId encoding).
    ///
    /// Unlike `revoke_capability_in_cnode` this:
    /// - performs **no heap allocation**
    /// - does not traverse delegation trees
    /// - clears the cnode slot and bumps its generation to invalidate stale CapIds
    /// - does not adjust memory-object refcounts (Reply caps have none)
    /// - does not remove delegation links (Reply caps are never delegated)
    ///
    /// Returns `true` if the slot was cleared, `false` otherwise.
    /// Callers must treat `false` as a diagnostic indication only — a `false`
    /// result must never prevent or undo an already-delivered reply.
    pub(crate) fn fast_revoke_reply_cap_in_cnode(
        &mut self,
        cnode: CNodeId,
        cap: CapId,
        expected_object: CapObject,
    ) -> bool {
        self.with_capability_state_mut(|capability_state| {
            capability_state
                .cnode_spaces
                .iter_mut()
                .flatten()
                .find(|space| space.id == cnode)
                .map(|space| kernel_mut(&mut space.cspace).fast_revoke_reply_slot(cap, expected_object))
                .unwrap_or(false)
        })
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
        for delegated in descendants.iter().copied() {
            self.revoke_capability_direct_in_process_cnode(delegated.pid, delegated.cap);
        }
        self.remove_delegation_links_for(root, &descendants);
        self.revoke_active_transfer_mappings_for_cap(source_pid, cap);
        if let Some(capability) = source_capability {
            self.adjust_memory_object_cap_refcount(capability.object, -1);
            self.reclaim_memory_object_if_unreferenced(capability.object);
            self.destroy_notification_for_revoked_cap(capability.object);
        }
        Ok(())
    }

    /// Stage 22: tear down a Notification object whose cap was just revoked.
    ///
    /// Notification caps are single-owner per object (the creator mints exactly a
    /// SIGNAL + a RECEIVE cap into its own cnode; Notification caps are never
    /// granted cross-process and carry no refcount — see `create_notification`).
    /// Revoking ANY Notification cap therefore destroys the underlying object.
    ///
    /// Lock-rank: the caller (`revoke_capability_in_cnode` /
    /// `revoke_capability_direct_in_process_cnode`) has already released
    /// `capability_state_lock` (rank 4) before reaching here; `destroy_notification`
    /// acquires `ipc_state_lock` (rank 3) on its own, preserving cap→ipc ordering.
    ///
    /// Idempotent: the paired second cap (or a double-revoke) re-enters with the
    /// object slot already `None`; `destroy_notification` then returns
    /// `WrongObject`, which is swallowed here as a benign no-op. The snapshotted
    /// waiter (if any) is unblocked outside both locks via
    /// `wake_destroyed_notification_waiter`.
    fn destroy_notification_for_revoked_cap(&mut self, object: CapObject) {
        let CapObject::Notification { index, .. } = object else {
            return;
        };
        match self.destroy_notification(index) {
            Ok(Some(waiter_tid)) => {
                let _ = self.wake_destroyed_notification_waiter(waiter_tid);
            }
            // Object already gone (paired cap / double-revoke) or out of range:
            // benign no-op — nothing left to tear down.
            Ok(None) | Err(_) => {}
        }
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
            self.destroy_notification_for_revoked_cap(capability.object);
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
            // Stage 11: two-phase unmap. Absent pages silently skipped.
            // cap_refcount is decremented by the caller (revoke_capability_in_cnode /
            // revoke_capability_direct_in_process_cnode) AFTER this function returns,
            // so reclaim_memory_object_for_phys inside execute_tlb_shootdown_wait_plan
            // is a no-op (cap_refcount=1). Final reclaim happens when the caller calls
            // reclaim_memory_object_if_unreferenced after decrementing cap_refcount.
            if let Some(asid) = self.task_asid(mapping.owner_tid.0) {
                self.unmap_range_two_phase(asid, mapping.base.0 as usize, mapping.len);
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

    /// Find the CapId in the current task's cnode that holds a MemoryObject backed
    /// by the given physical address.
    ///
    /// Used by rollback_anon_map to locate and revoke caps for pages being unmapped
    /// during VmAnonMap failure recovery. Returns None if not found.
    ///
    /// Safe for freshly-created anonymous caps (no delegations, no transfer mappings)
    /// under the global lock.
    pub(crate) fn find_current_task_cap_for_memory_object_phys(
        &self,
        phys: PhysAddr,
    ) -> Option<(CNodeId, CapId)> {
        let cnode = self.current_task_cnode()?;
        let mo_id = self.with_memory_state(|memory| {
            memory
                .memory_objects
                .iter()
                .flatten()
                .find(|o| o.phys == phys)
                .map(|o| o.id)
        })?;
        let target_obj = CapObject::MemoryObject { id: mo_id };
        let cap_id = self.with_capability_state(|caps| {
            caps.cnode_spaces
                .iter()
                .flatten()
                .find(|space| space.id == cnode)
                .and_then(|space| {
                    let cspace = kernel_ref(&space.cspace);
                    cspace.live_cap_ids().find(|&id| {
                        cspace
                            .get(id)
                            .map(|cap| cap.object == target_obj)
                            .unwrap_or(false)
                    })
                })
        })?;
        Some((cnode, cap_id))
    }
}
