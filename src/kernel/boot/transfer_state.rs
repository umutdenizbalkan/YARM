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
            // 199G-C §2: the SHARED pin policy. An envelope owes exactly one object pin iff it
            // carries a shared-region descriptor — not because it carries a transfer capability.
            // The split envelope stash asks the same predicate, so the two stashes cannot drift.
            if Self::transfer_envelope_owes_pin(shared_region) {
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
        // 199G-C §2: the same shared policy the stash used, so acquire and release are one
        // decision read twice rather than two spellings that could diverge.
        if Self::transfer_envelope_owes_pin(envelope.shared_region) {
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

    /// Stage 198E2A: consume a transfer envelope WITHOUT releasing its shared-region object pin.
    ///
    /// Identical validation + one-shot consume to [`take_transfer_envelope`], but the `+1`
    /// MemoryObject pin (normally dropped for a shared-region envelope) is LEFT in place so the
    /// post-lock shared-region transaction snapshot can take over its lifetime ownership with no
    /// reference gap (the pin never transiently reaches zero). Non-shared envelopes carry no pin,
    /// so this is byte-identical to the plain consume for them.
    pub(crate) fn take_transfer_envelope_keep_pin(
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
        // NB: no `adjust_memory_object_pin_refcount(-1)` here — the caller keeps the pin.
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.transfer_records_materialized = ipc
                .telemetry
                .transfer_records_materialized
                .saturating_add(1);
            ipc.transfer_envelopes[idx] = None;
        });
        Some(envelope)
    }

    /// Stage 193D: non-consuming peek at a transfer envelope's source object type.
    ///
    /// Returns the `source_object` of the envelope named by `handle` (idx +
    /// generation), WITHOUT taking/transitioning the envelope or touching any
    /// refcount — a pure read used by the IpcSend reply-cap boundary split to route
    /// on object type (Reply vs ordinary), since the userspace IpcSend ABI carries
    /// no `FLAG_REPLY_CAP` and the kernel tags every transfer as `FLAG_CAP_TRANSFER`.
    /// `None` for an out-of-range / stale / vacant handle.
    pub(crate) fn peek_transfer_envelope_source_object(&self, handle: u64) -> Option<CapObject> {
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
        self.with_ipc_state(|ipc| ipc.transfer_envelopes[idx].map(|e| e.source_object))
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
        // Stage 173 (CAP-CNODE): default-off materialize-rollback markers.
        // Diagnostic only — the inverse-of-mint rollback is UNCHANGED.
        let cap_cnode = crate::kernel::boot::cap_cnode_enabled();
        if cap_cnode {
            crate::yarm_log!(
                "CAP_CNODE_MATERIALIZE_ROLLBACK_BEGIN tid={} slot={}",
                receiver_tid,
                materialized_cap.0
            );
        }
        let Some(receiver_cnode) = self.task_cnode(receiver_tid) else {
            return false;
        };
        let Some(cap_object) = self
            .capability_for_cnode_local(receiver_cnode, materialized_cap)
            .map(|cap| cap.object)
        else {
            return false;
        };
        // Stage 198D2A: the rollback FLAVOR is object-authoritative, derived from the
        // resolved receiver-local cap object, not the caller's flag-derived
        // `is_reply_cap`. A queued IpcSend reply cap is minted (via the object-routed
        // reply path) from a FLAG_CAP_TRANSFER message, so its caller passes
        // `is_reply_cap == false`; using that here would wrongly run the transfer
        // rollback on a Reply cap. The `is_reply_cap` parameter is retained for the
        // diagnostic markers below but no longer selects the reclamation path.
        let _ = is_reply_cap;
        if matches!(cap_object, CapObject::Reply { .. }) {
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
            if cap_cnode {
                if cleared {
                    crate::yarm_log!(
                        "CAP_CNODE_MATERIALIZE_ROLLBACK_OK tid={} slot={}",
                        receiver_tid,
                        materialized_cap.0
                    );
                } else {
                    crate::yarm_log!(
                        "CAP_CNODE_ROLLBACK_LEAK tid={} slot={} kind=reply",
                        receiver_tid,
                        materialized_cap.0
                    );
                }
            }
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
            if cap_cnode {
                if ok {
                    crate::yarm_log!(
                        "CAP_CNODE_MATERIALIZE_ROLLBACK_OK tid={} slot={}",
                        receiver_tid,
                        materialized_cap.0
                    );
                } else {
                    crate::yarm_log!(
                        "CAP_CNODE_ROLLBACK_LEAK tid={} slot={} kind=transfer",
                        receiver_tid,
                        materialized_cap.0
                    );
                }
            }
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
        // 199G-C4 §2: the bound check itself is THE shared policy — the split envelope stash asks
        // the same function with the same inputs, so the two stashes cannot drift on what a
        // shared-region descriptor is allowed to name. Only the LOOKUP of the backing object's
        // length differs between them (broad `with_memory_state` here, the rank-6 split seam
        // there), which is why it is passed in rather than performed inside.
        let object_len = self.with_memory_state(|memory| {
            KernelState::shared_region_object_len_locked(memory, capability.object)
        });
        transfer_shared_region_bounds_ok(capability.object, region, object_len)
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
        self.with_ipc_state(|ipc| ipc.endpoint_waiter_tid(index))
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

    /// Stage 198E3B2A: `&mut IpcSubsystem` sibling of [`Self::register_active_transfer_mapping`] for
    /// use inside `SharedKernel::with_ipc_split_mut` (rank 3 only). Byte-identical slot logic.
    pub(crate) fn register_active_transfer_mapping_locked(
        ipc: &mut super::IpcSubsystem,
        owner_tid: ThreadId,
        transfer_cap: CapId,
        base: VirtAddr,
        len: usize,
    ) -> bool {
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
            true
        } else {
            false
        }
    }

    /// Stage 198E3B2A: update the provisional active-mapping entry's authorized length to the
    /// current mapped prefix (rank 3 only) — the domain-side "update mapped-prefix progress" seam.
    /// Returns true iff the entry existed. (The accepted authoritative progress is the txn-local
    /// `mapped_prefix_len`; this keeps the registry entry consistent for prefix-tracked cleanup.)
    pub(crate) fn update_active_transfer_mapping_len_locked(
        ipc: &mut super::IpcSubsystem,
        owner_tid: ThreadId,
        transfer_cap: CapId,
        new_len: usize,
    ) -> bool {
        for slot in ipc.active_transfer_mappings.iter_mut() {
            if let Some(mapping) = slot.as_mut() {
                if mapping.owner_tid == owner_tid && mapping.transfer_cap == transfer_cap {
                    mapping.len = new_len;
                    return true;
                }
            }
        }
        false
    }

    /// Stage 198E3B2A: `&mut IpcSubsystem` sibling of [`Self::remove_active_transfer_mapping`]
    /// (rank 3 only). Byte-identical guarded remove.
    pub(crate) fn remove_active_transfer_mapping_locked(
        ipc: &mut super::IpcSubsystem,
        owner_tid: ThreadId,
        transfer_cap: CapId,
    ) -> bool {
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

/// SHARED-REGION-OFFSET1 — what a shared-region descriptor selects, once its source admits it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct SharedRegionSelection {
    /// The object-relative byte offset of the first selected byte. Page-aligned by construction.
    pub(crate) object_offset: u64,
    /// The exact LOGICAL length the descriptor names, in bytes. What the receiver is told.
    pub(crate) len: u64,
    /// The page-rounded length that is mapped, registered and released. Never smaller than
    /// `len`, never more than one page short of a page beyond it.
    pub(crate) mapped_len: u64,
}

/// Why a descriptor names nothing its source can back. Each is a distinct fact so a caller can
/// choose its own error class; none is a partial answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SharedRegionRangeRefusal {
    /// Not a `MemoryObject` or `DmaRegion` capability.
    NotARegion,
    /// `len == 0`.
    Empty,
    /// `offset` is not page-aligned. Refused, never rounded down.
    Misaligned,
    /// Rounding `len` up to a page, `offset + mapped_len`, or the window's end overflows `u64`.
    Overflow,
    /// A `DmaRegion` window that does not lie inside its object.
    WindowOutsideObject,
    /// The page-rounded selection reaches outside the capability's window.
    OutsideWindow,
}

/// SHARED-REGION-OFFSET1 — THE shared-region range policy. Every owner that admits, maps or
/// releases a shared region asks this one function: both envelope stashes and both NR 1 send
/// arms (admission), and the per-page frame owner every receive family maps through
/// ([`KernelState::shared_region_page_phys_locked`]).
///
/// * `offset` and `len` are BYTES. `offset` is relative to the underlying `MemoryObject` — not a
///   page index, and not a sender or receiver virtual address.
/// * The capability's WINDOW is what the selection must lie inside: the whole object,
///   `[0, object_len)`, for a `MemoryObject`; `[window.offset, window.offset + window.len)` for a
///   `DmaRegion`, which must itself lie inside its object. A `DmaRegion` restricts the
///   object-relative interval; it does not rebase it, so its base is never added to `offset`.
/// * `offset` must be page-aligned: a mapping starts on a frame boundary, and rounding a
///   misaligned offset down would expose bytes before the ones the sender named.
/// * The final page. A mapping is page-granular, so a `len` that is not a page multiple is mapped
///   as `mapped_len = round_up(len)`, and the receiver can read the bytes between `offset + len`
///   and the end of that page. That is admissible only because the WHOLE page-rounded selection
///   must lie inside the window: every object and every `DmaRegion` window is page-granular, so
///   the page holding the last selected byte belongs to the same authorized backing. No
///   byte-level isolation of that tail is claimed.
///
/// Every end and every rounding is checked arithmetic. `object_len` is the backing object's
/// length in bytes, read by whichever owner the caller holds.
pub(crate) fn shared_region_selection(
    object: CapObject,
    region: TransferSharedRegion,
    object_len: u64,
) -> Result<SharedRegionSelection, SharedRegionRangeRefusal> {
    let (window_start, window_len) = match object {
        CapObject::MemoryObject { .. } => (0, object_len),
        CapObject::DmaRegion { offset, len, .. } => (offset, len),
        _ => return Err(SharedRegionRangeRefusal::NotARegion),
    };
    if region.len == 0 {
        return Err(SharedRegionRangeRefusal::Empty);
    }
    let page = crate::kernel::vm::PAGE_SIZE as u64;
    if !region.offset.is_multiple_of(page) {
        return Err(SharedRegionRangeRefusal::Misaligned);
    }
    let mapped_len = region
        .len
        .checked_next_multiple_of(page)
        .ok_or(SharedRegionRangeRefusal::Overflow)?;
    let mapped_end = region
        .offset
        .checked_add(mapped_len)
        .ok_or(SharedRegionRangeRefusal::Overflow)?;
    let window_end = window_start
        .checked_add(window_len)
        .ok_or(SharedRegionRangeRefusal::Overflow)?;
    if window_end > object_len {
        return Err(SharedRegionRangeRefusal::WindowOutsideObject);
    }
    if region.offset < window_start || mapped_end > window_end {
        return Err(SharedRegionRangeRefusal::OutsideWindow);
    }
    Ok(SharedRegionSelection {
        object_offset: region.offset,
        len: region.len,
        mapped_len,
    })
}

/// The `KernelError` each range refusal is reported as by the kernel-internal owners: a
/// misaligned offset is `Vm(Misaligned)`, anything else is a descriptor its source cannot back.
pub(crate) const fn shared_region_refusal_error(refusal: SharedRegionRangeRefusal) -> KernelError {
    match refusal {
        SharedRegionRangeRefusal::Misaligned => KernelError::Vm(VmError::Misaligned),
        _ => KernelError::WrongObject,
    }
}

/// 199G-C4 §2 — THE shared-region admission check, shared by the broad and split envelope stashes
/// and by both NR 1 send arms.
///
/// Given the resolved source object, the descriptor the sender named, and the backing object's
/// length as read by whichever rank-6 owner the caller had available, decide whether the
/// descriptor is admissible. Pure: it reads no kernel state, so every caller reaches the same
/// verdict from the same facts. The rules are [`shared_region_selection`]'s; a stale object
/// (`object_len == None`) is `MemoryObjectMissing`.
pub(crate) fn transfer_shared_region_bounds_ok(
    object: CapObject,
    region: TransferSharedRegion,
    object_len: Option<usize>,
) -> Result<(), KernelError> {
    if !matches!(
        object,
        CapObject::MemoryObject { .. } | CapObject::DmaRegion { .. }
    ) {
        return Err(KernelError::WrongObject);
    }
    let object_len = object_len.ok_or(KernelError::MemoryObjectMissing)?;
    let object_len = u64::try_from(object_len).map_err(|_| KernelError::WrongObject)?;
    shared_region_selection(object, region, object_len)
        .map(|_| ())
        .map_err(shared_region_refusal_error)
}
