// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;
use alloc::boxed::Box;

impl KernelState {
    pub fn current_task_cnode(&self) -> Option<CNodeId> {
        let tid = self.current_tid()?;
        self.task_cnode(tid)
    }

    pub fn task_cnode(&self, tid: u64) -> Option<CNodeId> {
        self.with_task_then_capability(|tcbs, capability| {
            let pid = tcbs
                .iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.thread_group_id.0)?;
            capability
                .process_cnodes
                .iter()
                .flatten()
                .find(|record| record.pid == pid)
                .map(|record| record.cnode)
        })
    }

    pub(crate) fn process_cnode_for_pid(&self, pid: u64) -> Option<CNodeId> {
        self.with_capability_state(|capability| {
            capability
                .process_cnodes
                .iter()
                .flatten()
                .find(|record| record.pid == pid)
                .map(|record| record.cnode)
        })
    }

    pub(crate) fn set_process_cnode_for_pid(
        &mut self,
        pid: u64,
        cnode: CNodeId,
    ) -> Result<(), KernelError> {
        self.with_capability_state_mut(|capability| {
            if let Some(record) = capability
                .process_cnodes
                .iter_mut()
                .flatten()
                .find(|record| record.pid == pid)
            {
                record.cnode = cnode;
                return Ok(());
            }
            if let Some(slot) = capability
                .process_cnodes
                .iter_mut()
                .find(|slot| slot.is_none())
            {
                *slot = Some(ProcessCNodeRecord { pid, cnode });
                return Ok(());
            }
            Err(KernelError::TaskTableFull)
        })
    }

    pub(crate) fn maybe_cleanup_process_cnode_for_pid(&mut self, pid: u64) {
        #[derive(Default)]
        struct ProcessCnodeCleanupTelemetry {
            revoked_caps: usize,
            removed_delegation_links: usize,
            removed_cnode_space: bool,
            removed_process_record: bool,
        }

        let has_live_threads = self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .any(|tcb| tcb.thread_group_id.0 == pid && tcb.status != TaskStatus::Dead)
        });
        if has_live_threads {
            return;
        }
        let mut reclaimed_asids = [None; MAX_TASKS];
        let mut reclaimed_asid_len = 0usize;
        self.with_tcbs(|tcbs| {
            for tcb in tcbs.iter().flatten() {
                if tcb.thread_group_id.0 != pid {
                    continue;
                }
                let Some(asid) = tcb.asid else {
                    continue;
                };
                if reclaimed_asids
                    .iter()
                    .take(reclaimed_asid_len)
                    .flatten()
                    .any(|candidate| *candidate == asid)
                {
                    continue;
                }
                if reclaimed_asid_len < reclaimed_asids.len() {
                    reclaimed_asids[reclaimed_asid_len] = Some(asid);
                    reclaimed_asid_len += 1;
                }
            }
        });
        for asid in reclaimed_asids
            .into_iter()
            .take(reclaimed_asid_len)
            .flatten()
        {
            let _ = self.destroy_user_address_space_by_asid(asid);
        }
        self.purge_transfer_envelopes_for_pid(pid);
        self.purge_active_transfer_mappings_for_pid(pid);
        let Some(cnode) = self.process_cnode_for_pid(pid) else {
            return;
        };
        let cnode_slot_capacity = self.cnode_slot_capacity(cnode).unwrap_or(0);
        let mut telemetry = ProcessCnodeCleanupTelemetry::default();

        loop {
            let next_live_cap = self.with_capability_state(|capability| {
                capability
                    .cnode_spaces
                    .iter()
                    .flatten()
                    .find(|space| space.id == cnode)
                    .and_then(|space| kernel_ref(&space.cspace).live_cap_ids().next())
            });
            let Some(cap) = next_live_cap else {
                break;
            };
            if self.revoke_capability_in_cnode(cnode, cap).is_ok() {
                telemetry.revoked_caps = telemetry.revoked_caps.saturating_add(1);
            }
        }

        let link_snapshot = Box::new(
            self.with_capability_state(|capability| capability.delegated_capability_links.clone()),
        );
        let mut remove_links = Box::new([false; MAX_DELEGATED_CAPABILITY_LINKS]);
        let mut removed_delegation_links = 0usize;
        for (idx, maybe_record) in link_snapshot.iter().enumerate() {
            let Some(record) = maybe_record else {
                continue;
            };
            let source_pid = self
                .process_id(record.source_tid)
                .unwrap_or(record.source_tid);
            let dest_pid = self.process_id(record.dest_tid).unwrap_or(record.dest_tid);
            if source_pid == pid || dest_pid == pid {
                remove_links[idx] = true;
                removed_delegation_links = removed_delegation_links.saturating_add(1);
            }
        }
        self.with_capability_state_mut(|capability| {
            for (idx, remove) in remove_links.iter().enumerate() {
                if *remove {
                    capability.delegated_capability_links[idx] = None;
                }
            }
        });
        telemetry.removed_delegation_links = telemetry
            .removed_delegation_links
            .saturating_add(removed_delegation_links);
        self.with_capability_state_mut(|capability| {
            if let Some(slot) = capability
                .cnode_spaces
                .iter_mut()
                .find(|slot| slot.as_ref().is_some_and(|space| space.id == cnode))
            {
                *slot = None;
                telemetry.removed_cnode_space = true;
            }

            if let Some(slot) = capability
                .process_cnodes
                .iter_mut()
                .find(|slot| slot.is_some_and(|record| record.pid == pid))
            {
                *slot = None;
                telemetry.removed_process_record = true;
            }
        });

        crate::yarm_log!(
            "YARM_PROC_CNODE_CLEANUP pid={} cnode={} slots={} revoked_caps={} removed_links={} removed_cspace={} removed_record={}",
            pid,
            cnode.0,
            cnode_slot_capacity,
            telemetry.revoked_caps,
            telemetry.removed_delegation_links,
            telemetry.removed_cnode_space as u8,
            telemetry.removed_process_record as u8
        );
    }

    pub(crate) fn purge_transfer_envelopes_for_pid(&mut self, pid: u64) {
        for idx in 0..MAX_TRANSFER_ENVELOPES {
            let envelope = self.with_ipc_state(|ipc| ipc.transfer_envelopes[idx]);
            let Some(envelope) = envelope else {
                continue;
            };
            let source_pid = self
                .process_id(envelope.source_tid.0)
                .unwrap_or(envelope.source_tid.0);
            let receiver_pid = envelope
                .receiver_tid
                .map(|tid| self.process_id(tid.0).unwrap_or(tid.0));
            let source_matches = source_pid == pid || envelope.source_tid.0 == pid;
            let receiver_matches =
                receiver_pid == Some(pid) || envelope.receiver_tid == Some(ThreadId(pid));
            if !source_matches && !receiver_matches {
                continue;
            }
            if matches!(envelope.source_object, CapObject::MemoryObject { .. }) {
                self.adjust_memory_object_pin_refcount(envelope.source_object, -1);
            }
            self.with_ipc_state_mut(|ipc| ipc.transfer_envelopes[idx] = None);
            self.note_transfer_record_revoked();
        }
    }

    pub(crate) fn purge_active_transfer_mappings_for_pid(&mut self, pid: u64) {
        for idx in 0..MAX_TRANSFER_ENVELOPES {
            let mapping = self.with_ipc_state(|ipc| ipc.active_transfer_mappings[idx]);
            let Some(mapping) = mapping else {
                continue;
            };
            let owner_pid = self
                .process_id(mapping.owner_tid.0)
                .unwrap_or(mapping.owner_tid.0);
            if owner_pid != pid && mapping.owner_tid.0 != pid {
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
            if let Some(cnode) = self.task_cnode(mapping.owner_tid.0) {
                let _ = self.revoke_capability_in_cnode(cnode, mapping.transfer_cap);
            }
            self.note_transfer_record_revoked();
        }
    }
}
