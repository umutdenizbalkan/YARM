// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

impl KernelState {
    pub(crate) fn stash_transfer_envelope(
        &mut self,
        source_tid: ThreadId,
        source_cap: CapId,
        endpoint: CapObject,
        receiver_tid: Option<ThreadId>,
        shared_region: Option<TransferSharedRegion>,
    ) -> Option<u64> {
        for idx in 0..MAX_TRANSFER_ENVELOPES {
            if self.with_ipc_state(|ipc| ipc.transfer_envelopes[idx].is_some()) {
                continue;
            }
            let mut generation = self
                .with_ipc_state(|ipc| ipc.transfer_envelope_generations[idx])
                .wrapping_add(1);
            if generation == 0 {
                generation = 1;
            }
            if self
                .validate_transfer_record_metadata(source_tid, source_cap, shared_region)
                .is_err()
            {
                self.with_ipc_state_mut(|ipc| {
                    ipc.telemetry.transfer_record_failures =
                        ipc.telemetry.transfer_record_failures.saturating_add(1);
                });
                return None;
            }
            let source_object = self
                .resolve_capability_for_task(source_tid.0, source_cap)
                .ok()?
                .object;
            if shared_region.is_some() {
                self.adjust_memory_object_pin_refcount(source_object, 1);
            }
            self.with_ipc_state_mut(|ipc| {
                ipc.transfer_envelope_generations[idx] = generation;
                ipc.transfer_envelopes[idx] = Some(TransferEnvelope {
                    source_tid,
                    source_cap,
                    source_object,
                    endpoint,
                    receiver_tid,
                    state: TransferState::Created,
                    shared_region,
                    generation,
                });
                ipc.telemetry.transfer_records_created =
                    ipc.telemetry.transfer_records_created.saturating_add(1);
            });
            let idx_part = u64::try_from(idx).ok()?;
            return Some((generation << 16) | idx_part);
        }
        None
    }

    pub(crate) fn take_transfer_envelope(
        &mut self,
        handle: u64,
        endpoint: CapObject,
        receiver_tid: ThreadId,
    ) -> Option<TransferEnvelope> {
        let idx = usize::try_from(handle & 0xFFFF).ok()?;
        if idx >= MAX_TRANSFER_ENVELOPES {
            return None;
        }
        let generation = handle >> 16;
        if generation == 0
            || self.with_ipc_state(|ipc| ipc.transfer_envelope_generations[idx]) != generation
        {
            return None;
        }
        let mut envelope = self.with_ipc_state(|ipc| ipc.transfer_envelopes[idx])?;
        if envelope.endpoint != endpoint {
            return None;
        }
        if let Some(bound_receiver) = envelope.receiver_tid {
            if bound_receiver != receiver_tid {
                return None;
            }
        }
        envelope = envelope.transition(TransferState::Released)?;
        if envelope.shared_region.is_some() {
            self.adjust_memory_object_pin_refcount(envelope.source_object, -1);
        }
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.transfer_records_materialized = ipc
                .telemetry
                .transfer_records_materialized
                .saturating_add(1);
            ipc.transfer_envelopes[idx] = None;
        });
        Some(envelope)
    }

    /// Stage 20: roll back a Reply/transfer cap that was just materialized into a
    /// receiver's cnode when a *subsequent* user-memory copy in the recv-delivery
    /// path fails.
    ///
    /// The delivery path materializes the cap (minting it into the receiver's cnode
    /// and consuming the transfer envelope) *before* the metadata/payload `copy_to_user`
    /// that may fault.  If that copy faults the message is dropped and the receiver
    /// stays blocked, but without this rollback the freshly-minted cap would leak in
    /// the receiver's cnode (and, for Reply caps, leave a dangling `waiter_cap_id` in
    /// the global record) — an asymmetric `cap_refcount`/cnode-slot leak on every
    /// faulting delivery.
    ///
    /// This is the inverse of `mint_capability_in_cnode` for the materialized slot:
    ///   - Reply cap: clear the receiver cnode slot via `fast_revoke_reply_cap_in_cnode`
    ///     (no `cap_refcount` to adjust) and clear the global `waiter_cap_id` so the
    ///     record no longer points at a now-revoked slot.  The reply remains live and
    ///     re-deliverable (the global `ReplyCapRecord` was never consumed by mint).
    ///   - Transfer cap (MemoryObject/DmaRegion): `revoke_capability_in_cnode`, which
    ///     removes the delegation link, decrements `cap_refcount`, and reclaims the
    ///     object if it became unreferenced — exactly undoing the materialization
    ///     mint+link.
    ///
    /// The materialized object is resolved from the receiver's cnode so callers only
    /// need the receiver tid, the minted CapId, and whether it is a Reply cap.
    /// Returns `true` if a slot was cleared.
    pub(crate) fn rollback_materialized_recv_cap(
        &mut self,
        receiver_tid: u64,
        materialized_cap: CapId,
        is_reply_cap: bool,
    ) -> bool {
        let Some(receiver_cnode) = self.task_cnode(receiver_tid) else {
            return false;
        };
        let Some(cap_object) = self
            .capability_for_cnode_local(receiver_cnode, materialized_cap)
            .map(|cap| cap.object)
        else {
            return false;
        };
        if is_reply_cap {
            let cleared =
                self.fast_revoke_reply_cap_in_cnode(receiver_cnode, materialized_cap, cap_object);
            if let CapObject::Reply { index, generation } = cap_object {
                // Drop the now-stale waiter_cap_id so ipc_reply does not try to
                // fast-revoke a slot we just cleared.
                self.clear_reply_cap_waiter_cap(index, generation);
            }
            crate::yarm_log!(
                "IPC_RECV_MATERIALIZE_ROLLBACK kind=reply receiver_tid={} cap={} cleared={}",
                receiver_tid,
                materialized_cap.0,
                cleared
            );
            cleared
        } else {
            let ok = self
                .revoke_capability_in_cnode(receiver_cnode, materialized_cap)
                .is_ok();
            crate::yarm_log!(
                "IPC_RECV_MATERIALIZE_ROLLBACK kind=transfer receiver_tid={} cap={} ok={}",
                receiver_tid,
                materialized_cap.0,
                ok
            );
            ok
        }
    }

    fn validate_transfer_record_metadata(
        &self,
        source_tid: ThreadId,
        source_cap: CapId,
        shared_region: Option<TransferSharedRegion>,
    ) -> Result<(), KernelError> {
        let capability = self.resolve_capability_for_task(source_tid.0, source_cap)?;
        let Some(region) = shared_region else {
            return Ok(());
        };
        if region.len == 0 {
            return Err(KernelError::WrongObject);
        }
        let end = region
            .offset
            .checked_add(region.len)
            .ok_or(KernelError::WrongObject)?;
        match capability.object {
            CapObject::MemoryObject { id } => {
                let mem = self
                    .with_memory_state(|memory| {
                        memory
                            .memory_objects
                            .iter()
                            .flatten()
                            .find(|entry| entry.id == id)
                            .copied()
                    })
                    .ok_or(KernelError::MemoryObjectMissing)?;
                let max_len = u64::try_from(mem.len).map_err(|_| KernelError::WrongObject)?;
                if region.len > max_len || end < region.offset {
                    return Err(KernelError::WrongObject);
                }
            }
            CapObject::DmaRegion {
                offset: base,
                len: span,
                ..
            } => {
                let cap_end = base.checked_add(span).ok_or(KernelError::WrongObject)?;
                if region.offset < base || end > cap_end {
                    return Err(KernelError::WrongObject);
                }
            }
            _ => return Err(KernelError::WrongObject),
        }
        Ok(())
    }

    pub fn endpoint_waiter_tid(&self, endpoint: CapObject) -> Option<ThreadId> {
        let CapObject::Endpoint { index, generation } = endpoint else {
            return None;
        };
        if index >= MAX_ENDPOINTS {
            return None;
        }
        if self.with_ipc_state(|ipc| ipc.endpoint_generations[index]) != generation {
            return None;
        }
        self.with_ipc_state(|ipc| ipc.endpoint_waiters[index])
    }

    pub(crate) fn note_transfer_record_revoked(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.transfer_records_revoked =
                ipc.telemetry.transfer_records_revoked.saturating_add(1);
        });
    }

    pub(crate) fn note_shared_mem_mapped(&mut self, len: usize) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.shared_mem_bytes_mapped = ipc
                .telemetry
                .shared_mem_bytes_mapped
                .saturating_add(len as u64);
        });
    }

    pub(crate) fn note_shared_mem_released(&mut self, len: usize) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.transfer_release_calls =
                ipc.telemetry.transfer_release_calls.saturating_add(1);
            ipc.telemetry.shared_mem_bytes_released = ipc
                .telemetry
                .shared_mem_bytes_released
                .saturating_add(len as u64);
        });
    }

    pub(crate) fn register_active_transfer_mapping(
        &mut self,
        owner_tid: ThreadId,
        transfer_cap: CapId,
        base: VirtAddr,
        len: usize,
    ) -> Result<(), KernelError> {
        self.with_ipc_state_mut(|ipc| {
            if let Some(slot) = ipc
                .active_transfer_mappings
                .iter_mut()
                .find(|slot| slot.is_none())
            {
                *slot = Some(ActiveTransferMapping {
                    owner_tid,
                    transfer_cap,
                    base,
                    len,
                });
                Ok(())
            } else {
                Err(KernelError::EndpointFull)
            }
        })
    }

    pub(crate) fn remove_active_transfer_mapping(
        &mut self,
        owner_tid: ThreadId,
        transfer_cap: CapId,
    ) -> bool {
        self.with_ipc_state_mut(|ipc| {
            for slot in ipc.active_transfer_mappings.iter_mut() {
                let Some(mapping) = *slot else {
                    continue;
                };
                if mapping.owner_tid == owner_tid && mapping.transfer_cap == transfer_cap {
                    *slot = None;
                    return true;
                }
            }
            false
        })
    }

    pub(crate) fn active_transfer_mapping_for(
        &self,
        owner_tid: ThreadId,
        transfer_cap: CapId,
    ) -> Option<(VirtAddr, usize)> {
        self.with_ipc_state(|ipc| {
            ipc.active_transfer_mappings
                .iter()
                .flatten()
                .find(|mapping| {
                    mapping.owner_tid == owner_tid && mapping.transfer_cap == transfer_cap
                })
                .map(|mapping| (mapping.base, mapping.len))
        })
    }
}
