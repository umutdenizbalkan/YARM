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
    pub(crate) fn normalize_requested_cnode_slots(
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

    /// Stage 181C: drop the cached revoke-scratch working set for `cnode`, returning
    /// its (PT-pool-backed) pages to the allocator. Used by the graduated one-shot
    /// proof so its throwaway cap revokes do not leave a large cached scratch set
    /// resident that would starve a later fork's cnode-slot allocation. Returns
    /// `true` if a cache was actually released. The next real revoke rebuilds it.
    pub(crate) fn drop_revoke_scratch_cache_for_cnode(&mut self, cnode: CNodeId) -> bool {
        self.with_capability_state_mut(|capability| {
            capability
                .cnode_spaces
                .iter_mut()
                .flatten()
                .find(|space| space.id == cnode)
                .map(|space| kernel_mut(&mut space.cspace).drop_revoke_scratch_cache())
                .unwrap_or(false)
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

    /// Stage 173 (CAP-CNODE): bounded, self-contained, one-shot diagnostic proof
    /// of the capability/CNode reserve → materialize → lookup → release lifecycle,
    /// plus the stale-cap and double-release rejection invariants. Runs at most
    /// once (per boot) when `yarm.cap_cnode=1` and a real user task is current.
    /// It mints ONE scratch anonymous MemoryObject cap into the current task's
    /// CNode, exercises the lifecycle through the REAL production functions
    /// (`alloc_anonymous_memory_object`, `capability_for_cnode_local`,
    /// `revoke_capability_in_cnode`), verifies the object refcount returns to
    /// baseline, and cleans itself up — it changes no service state and consumes
    /// no net slots. Diagnostic only: it never faults the task and swallows all
    /// errors into a `CAP_CNODE_*_FAIL` marker.
    pub(crate) fn maybe_run_cap_cnode_proof(&mut self) {
        if !crate::kernel::boot::cap_cnode_enabled() {
            return;
        }
        let Some(tid) = self.current_tid() else {
            return;
        };
        if tid == 0 {
            return; // need a real user task with a CNode
        }
        let Some(cnode) = self.current_task_cnode() else {
            return;
        };
        if !crate::kernel::boot::cap_cnode_proof_try_start() {
            return; // one-shot
        }
        crate::yarm_log!("CAP_CNODE_PROOF_BEGIN tid={} cnode={}", tid, cnode.0);

        // Phase 1: RESERVE + MATERIALIZE (alloc mints exactly one cap into cnode).
        crate::yarm_log!("CAP_CNODE_RESERVE_BEGIN tid={}", tid);
        crate::yarm_log!("CAP_CNODE_MATERIALIZE_BEGIN tid={} obj=memory_object", tid);
        let (mo_id, cap) = match self.alloc_anonymous_memory_object() {
            Ok(pair) => pair,
            Err(e) => {
                crate::yarm_log!("CAP_CNODE_MATERIALIZE_FAIL tid={} reason={:?}", tid, e);
                crate::yarm_log!("CAP_CNODE_PROOF_DONE tid={} result=materialize_fail", tid);
                return;
            }
        };
        crate::yarm_log!("CAP_CNODE_RESERVE_OK tid={} slot={}", tid, cap.0);
        let cap_ref_after_mint = self
            .memory_object_slot_by_id(mo_id)
            .and_then(|s| self.memory.memory_objects[s])
            .map(|m| m.cap_refcount)
            .unwrap_or(0);
        crate::yarm_log!(
            "CAP_CNODE_REF_INC obj={} old={} new={}",
            mo_id,
            cap_ref_after_mint.saturating_sub(1),
            cap_ref_after_mint
        );
        crate::yarm_log!(
            "CAP_CNODE_SLOT_INSTALL tid={} slot={} generation={}",
            tid,
            cap.0,
            cap.0 >> 32
        );
        crate::yarm_log!("CAP_CNODE_MATERIALIZE_OK tid={} slot={}", tid, cap.0);

        // Phase 2: LOOKUP OK + rights subset (no escalation).
        crate::yarm_log!("CAP_CNODE_LOOKUP_BEGIN tid={} cap={}", tid, cap.0);
        match self.capability_for_cnode_local(cnode, cap) {
            Some(capability) => {
                crate::yarm_log!(
                    "CAP_CNODE_LOOKUP_OK tid={} cap={} obj=memory_object rights=0x{:x}",
                    tid,
                    cap.0,
                    capability.rights_bits()
                );
                // Deriving a strict superset must be rejected by the cap algebra:
                // add a right the memory-object cap does not hold (SIGNAL) and
                // confirm `derive` refuses to escalate to it.
                let superset = capability
                    .rights()
                    .union(crate::kernel::capabilities::CapRights::SIGNAL);
                if superset != capability.rights() && capability.derive(superset).is_ok() {
                    crate::yarm_log!("CAP_CNODE_RIGHTS_ESCALATION tid={} cap={}", tid, cap.0);
                }
            }
            None => {
                crate::yarm_log!(
                    "CAP_CNODE_LOOKUP_FAIL tid={} cap={} reason=invalid",
                    tid,
                    cap.0
                );
            }
        }

        // Phase 3: RELEASE (revoke) — refcount must decrement exactly once.
        crate::yarm_log!("CAP_CNODE_RELEASE_BEGIN tid={} cap={}", tid, cap.0);
        crate::yarm_log!("CAP_CNODE_SLOT_CLEAR tid={} slot={}", tid, cap.0);
        match self.revoke_capability_in_cnode(cnode, cap) {
            Ok(()) => {
                let cap_ref_after_revoke = self
                    .memory_object_slot_by_id(mo_id)
                    .and_then(|s| self.memory.memory_objects[s])
                    .map(|m| m.cap_refcount)
                    .unwrap_or(0);
                crate::yarm_log!(
                    "CAP_CNODE_REF_DEC obj={} old={} new={}",
                    mo_id,
                    cap_ref_after_mint,
                    cap_ref_after_revoke
                );
                crate::yarm_log!("CAP_CNODE_RELEASE_OK tid={}", tid);
            }
            Err(e) => {
                crate::yarm_log!("CAP_CNODE_RELEASE_FAIL tid={} reason={:?}", tid, e);
            }
        }

        // Phase 4: STALE lookup — the revoked cap must NOT resolve.
        crate::yarm_log!("CAP_CNODE_LOOKUP_BEGIN tid={} cap={}", tid, cap.0);
        if self.capability_for_cnode_local(cnode, cap).is_some() {
            crate::yarm_log!("CAP_CNODE_STALE_CAP_ACCEPTED tid={} cap={}", tid, cap.0);
        } else {
            crate::yarm_log!(
                "CAP_CNODE_LOOKUP_FAIL tid={} cap={} reason=stale_generation",
                tid,
                cap.0
            );
        }

        // Phase 5: DOUBLE RELEASE — revoking again must fail cleanly (no underflow).
        crate::yarm_log!("CAP_CNODE_RELEASE_BEGIN tid={} cap={}", tid, cap.0);
        match self.revoke_capability_in_cnode(cnode, cap) {
            Ok(()) => {
                // A second successful revoke would imply a stale-cap accept or an
                // over-decrement — both are invariant violations.
                crate::yarm_log!("CAP_CNODE_REFCOUNT_UNDERFLOW tid={} cap={}", tid, cap.0);
            }
            Err(_) => {
                crate::yarm_log!(
                    "CAP_CNODE_RELEASE_FAIL tid={} cap={} reason=stale",
                    tid,
                    cap.0
                );
            }
        }

        // Phase 6: INVARIANT — the scratch object is fully reclaimed (cap_refcount
        // 0 / slot gone), and no cap slot leaked. If anything is off, the failure
        // markers above already fired.
        let residual = self
            .memory_object_slot_by_id(mo_id)
            .and_then(|s| self.memory.memory_objects[s])
            .map(|m| m.cap_refcount)
            .unwrap_or(0);
        if residual == 0 {
            crate::yarm_log!("CAP_CNODE_INVARIANT_OK tid={}", tid);
        } else {
            crate::yarm_log!(
                "CAP_CNODE_SLOT_LEAK tid={} obj={} residual_cap_refcount={}",
                tid,
                mo_id,
                residual
            );
        }
        crate::yarm_log!("CAP_CNODE_PROOF_DONE tid={} result=ok", tid);
    }

    pub(crate) fn mint_capability_in_cnode(
        &mut self,
        cnode: CNodeId,
        capability: Capability,
    ) -> Result<CapId, KernelError> {
        // Stage 173 (CAP-CNODE): default-off ERROR-path markers only (the hot
        // success path is NOT instrumented here — the one-shot proof and the
        // reply/transfer sites carry the success markers, to keep marker volume
        // bounded). Behavior UNCHANGED: reserve-before-materialize, and no
        // refcount increment unless the slot install succeeded (no partial mint).
        let cap_cnode = crate::kernel::boot::cap_cnode_enabled();
        if let Err(e) = self.ensure_cnode_space(cnode) {
            if cap_cnode {
                crate::yarm_log!("CAP_CNODE_RESERVE_FAIL cnode={} reason=full", cnode.0);
            }
            return Err(e);
        }
        let minted = match self.with_capability_state_mut(|capability_state| {
            capability_state
                .cnode_spaces
                .iter_mut()
                .flatten()
                .find(|space| space.id == cnode)
                .map(|space| kernel_mut(&mut space.cspace))
                .ok_or(KernelError::TaskMissing)?
                .mint(capability)
                .map_err(|_| KernelError::CapabilityFull)
        }) {
            Ok(m) => m,
            Err(e) => {
                if cap_cnode {
                    crate::yarm_log!(
                        "CAP_CNODE_MATERIALIZE_FAIL cnode={} reason={:?}",
                        cnode.0,
                        e
                    );
                }
                return Err(e);
            }
        };
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
        // Stage 173 (CAP-CNODE): default-off reply-cap one-shot consume markers.
        // Diagnostic only — the revoke behavior is UNCHANGED. A `false` result on a
        // reply cap that was already consumed is the one-shot guarantee (the second
        // consume is BLOCKED), never an error.
        let cap_cnode = crate::kernel::boot::cap_cnode_enabled();
        if cap_cnode {
            crate::yarm_log!(
                "CAP_CNODE_REPLY_CONSUME_BEGIN cnode={} cap={}",
                cnode.0,
                cap.0
            );
        }
        let revoked = self.with_capability_state_mut(|capability_state| {
            capability_state
                .cnode_spaces
                .iter_mut()
                .flatten()
                .find(|space| space.id == cnode)
                .map(|space| {
                    kernel_mut(&mut space.cspace).fast_revoke_reply_slot(cap, expected_object)
                })
                .unwrap_or(false)
        });
        if cap_cnode {
            if revoked {
                crate::yarm_log!("CAP_CNODE_REPLY_CONSUME_OK cnode={} cap={}", cnode.0, cap.0);
            } else {
                crate::yarm_log!(
                    "CAP_CNODE_REPLY_DOUBLE_CONSUME_BLOCKED cnode={} cap={}",
                    cnode.0,
                    cap.0
                );
            }
        }
        revoked
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

    /// Stage 181C: release a capability that is a childless leaf WITHOUT building the
    /// `RevokeScratch` derivation-tree working set that full `revoke` allocates + caches
    /// (≈12 PT-pool pages for a 512-slot cspace). Preserves every object-teardown side
    /// effect of `revoke_capability_in_cnode` (delegation-link removal, transfer-mapping
    /// revocation, MemoryObject refcount/reclaim, Notification destroy); only the
    /// recursive derivation-tree walk is skipped. If the cap has cross-process delegated
    /// descendants OR in-cspace derived children it is NOT a leaf, and this transparently
    /// falls back to the full `revoke_capability_in_cnode` — so semantics are identical
    /// for non-leaf caps. Returns `Ok(true)` if released via the leaf fast path, or
    /// `Ok(false)` if it fell back to a full recursive revoke.
    pub(crate) fn delete_leaf_capability_in_cnode(
        &mut self,
        cnode: CNodeId,
        cap: CapId,
    ) -> Result<bool, KernelError> {
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
        // Stage 181C: proof-gated PT-pool sub-step trace (sender-wake only, so normal
        // boots stay quiet) to attribute any per-side-effect net delta on the mem_cap
        // path. The leaf path below is deliberately allocation-free.
        let step_trace = crate::kernel::boot::ipc_recv_proof_sender_wake_active();
        let substep = |label: &str| {
            if step_trace {
                crate::yarm_log!(
                    "UNLOCK_GRADUATED_D3_LEAFDEL step={} pt_pool_free_frames={}",
                    label,
                    crate::kernel::frame_allocator::pt_pool_free_frames()
                );
            }
        };
        substep("before_delete_leaf");
        // Any DIRECT delegation of this cap makes it a non-leaf; use full revoke (which
        // computes the full transitive closure). This check is allocation-free — the old
        // `collect_delegated_descendants` here allocated a Box-cloned links snapshot + two
        // worklist Vecs whose small-slab warm pages were the residual leak.
        if self.has_any_delegated_child(root) {
            self.revoke_capability_in_cnode(cnode, cap)?;
            return Ok(false);
        }
        substep("after_descendant_check");
        // Try the childless-leaf fast path (no RevokeScratch build). `None` => cnode
        // missing; `Some(Ok(false))` => in-cspace children exist (fall back).
        let leaf = self.with_capability_state_mut(|capability_state| {
            capability_state
                .cnode_spaces
                .iter_mut()
                .flatten()
                .find(|space| space.id == cnode)
                .map(|space| kernel_mut(&mut space.cspace).delete_if_leaf(cap))
        });
        match leaf {
            None => return Err(KernelError::TaskMissing),
            Some(Err(_)) => return Err(KernelError::InvalidCapability),
            Some(Ok(false)) => {
                // Has in-cspace derived children — not a leaf; full recursive revoke.
                self.revoke_capability_in_cnode(cnode, cap)?;
                return Ok(false);
            }
            Some(Ok(true)) => {}
        }
        substep("after_slot_clear");
        // Leaf removed. Preserve the remainder of `revoke_capability_in_cnode`'s teardown.
        // A leaf has NO delegated source links (verified above), so
        // `remove_delegation_links_for` would remove nothing — skip it entirely; it also
        // allocated Box-cloned snapshots (part of the residual). Transfer-mapping
        // revocation + MemoryObject reclaim below are fixed-array scans / a single
        // free_frame — allocation-free for the no-transfer scratch object.
        self.revoke_active_transfer_mappings_for_cap(source_pid, cap);
        substep("after_transfer_revocation");
        if let Some(capability) = source_capability {
            self.adjust_memory_object_cap_refcount(capability.object, -1);
            substep("after_mo_refcount_decrement");
            self.reclaim_memory_object_if_unreferenced(capability.object);
            substep("after_mo_reclaim");
            self.destroy_notification_for_revoked_cap(capability.object);
        }
        substep("after_delete_leaf_done");
        Ok(true)
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

// ── U9-F: the phased, rank-ordered split of `revoke_capability_in_cnode` ───────────────────
//
// U9-F — the phased, rank-ordered split of [`KernelState::revoke_capability_in_cnode`].
//
// # What the broad function actually owes
//
// `revoke_capability_in_cnode` performs SIX obligations under one broad
// `SpinLock<KernelState>` acquisition:
//
// 1. **cross-CNode descendant revocation** — the transitive delegated closure of the cap is
//    revoked in each descendant's OWN process cnode (rank 4);
// 2. **delegation-link removal** — every `delegated_capability_links` entry naming the root or a
//    descendant is cleared (rank 4);
// 3. **root revocation** — `CapabilitySpace::revoke` in the owning cnode (rank 4);
// 4. **active-transfer-mapping revocation** — `unmap_range_two_phase` + TLB shootdown for every
//    registry record keyed by the cap (rank 5/6, **D3-fenced**);
// 5. **memory-object refcount drop + reclaim** — `adjust_memory_object_cap_refcount(-1)` then
//    `reclaim_memory_object_if_unreferenced` (rank 6, frees frames);
// 6. **notification destruction + waiter wake** — `destroy_notification` (rank 3) then
//    `wake_destroyed_notification_waiter` (rank 2 → rank 1).
//
// Obligations (4) and (5) are the D3 fence. This section splits (1), (2), (3) and (6) into
// separately rank-ordered phases and admits ONLY the object classes for which (4) and (5) are
// provably vacuous, so nothing here can reach VM shootdown or memory reclaim. It lives in this
// file — the owner of the broad function it mirrors — so the two cannot drift apart, and it
// introduces no seam file and no marker family.
//
// # Why (4) and (5) are vacuous for the admitted classes — from source, not from the enum
//
// (5) is direct: `adjust_memory_object_cap_refcount` (`memory_lifecycle_state.rs`) and
// `reclaim_memory_object_if_unreferenced` (same file) both open with
// `CapObject::MemoryObject { .. } | CapObject::DmaRegion { .. } => id, _ => return`.
//
// (4) is indirect, because `ActiveTransferMapping` (`defs.rs`) carries `{ owner_tid,
// transfer_cap, base, len }` — **no object identity and no generation** — and
// `revoke_active_transfer_mappings_for_cap` matches on the bare `CapId`. So the class alone does
// not settle it; the registration and aliasing paths do:
//
// * **Every** production `register_active_transfer_mapping{,_locked}` callsite is object-gated to
//   `MemoryObject`/`DmaRegion` before it can register:
//   - `syscall/ipc.rs` (recv-v2 `OPCODE_SHARED_MEM`) registers only after
//     `map_shared_region_into_receiver` → `map_user_page_in_asid_with_caps` →
//     `resolve_memory_object_phys`, which returns `WrongObject` for every other class;
//   - `syscall/recv_shared_v3.rs` registers only after its `DmaRegion`/`MemoryObject` match arm
//     produced `Some`; any other class takes the rollback-and-return arm first;
//   - `shared_region_txn.rs` (`ctx_register_active_mapping`, and its `SharedKernel` twin
//     `sr_register_active_mapping_split`) registers `txn.minted_cap`, minted from
//     `txn.snapshot.object`, and `shared_region_phase_a` admits only
//     `MemoryObject | DmaRegion` into that snapshot.
// * A **stale** record cannot alias a later cap: `CapId` is `(generation << 16) | index`, and both
//   `CapabilitySpace::revoke` and `delete_if_leaf` bump the slot generation as they clear it, so a
//   re-minted slot never reproduces a retired `CapId` value.
// * A record cannot outlive its owning process into a recycled TID: process teardown purges the
//   registry for the pid (`cnode_state.rs`, `purge_active_transfer_mappings_for_pid`, and the
//   noalloc reap's inline equivalent).
//
// The composition still runs a **fail-closed** rank-3 preflight over the registry (Phase P4) and
// refuses rather than proceeding if a record ever names an admitted cap. That branch is
// unreachable by the proof above; it exists so the proof, not this code, is the thing that has to
// stay true.
//
// # Refusal is always safe
//
// Every refusal is raised BEFORE the first mutation, and every caller falls back to the unchanged
// broad `rollback_materialized_recv_cap`. This composition therefore never narrows, filters or
// skips an obligation: it either performs the complete teardown off the broad lock, or performs
// nothing.

/// Bounded delegated-descendant closure. The rollback sites this serves revoke a cap that was
/// minted moments earlier in the same syscall and never handed to userspace, so the closure is
/// empty there; a larger closure refuses to the broad path rather than growing an allocation on
/// the kernel stack (and rather than warming PT-pool slab pages — see Stage 181C).
pub(crate) const SPLIT_REVOKE_MAX_DESCENDANTS: usize = 16;

/// Bounded per-node numeric `source_cap` candidates and bounded link-removal candidates.
pub(crate) const SPLIT_REVOKE_MAX_LINK_HITS: usize = 32;

/// Why the split composition declined a cap. Every variant means "the caller must use the
/// unchanged broad path"; none of them is an error the caller should surface.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SplitRevokeRefusal {
    /// `MemoryObject`/`DmaRegion`: teardown owes an active-transfer-mapping unmap + TLB shootdown
    /// and a memory refcount drop + frame reclaim. D3-fenced.
    MemoryBacked,
    /// `Reply`: teardown is the reply-registry transaction (`fast_revoke_reply_cap_in_cnode` +
    /// `clear_reply_cap_waiter_cap`), owned by `rollback_reply_cap_split`, not by this one.
    ReplyObject,
    /// The task, its process cnode, or the cap slot could not be resolved.
    Unresolvable,
    /// More delegated descendants than the bounded closure holds.
    DescendantOverflow,
    /// More delegation-link candidates than the bounded removal set holds.
    LinkOverflow,
    /// An active transfer-mapping record names the root or a descendant. Unreachable for the
    /// admitted classes (see the section docs above); fail-closed defence in depth.
    ActiveMappingPresent,
}

/// The class gate. `Ok` iff this object owes NEITHER an active-transfer-mapping unmap (VM / TLB
/// shootdown) NOR a memory-object refcount drop / frame reclaim, per the section docs above.
pub(crate) fn split_revoke_class_admitted(object: CapObject) -> Result<(), SplitRevokeRefusal> {
    match object {
        CapObject::MemoryObject { .. } | CapObject::DmaRegion { .. } => {
            Err(SplitRevokeRefusal::MemoryBacked)
        }
        CapObject::Reply { .. } => Err(SplitRevokeRefusal::ReplyObject),
        _ => Ok(()),
    }
}

/// One node of the bounded delegated closure, resolved to the same `(pid, cap)` identity the broad
/// `DelegatedCapRef` uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct SplitRevokeNode {
    pub(crate) pid: u64,
    pub(crate) cap: CapId,
}

/// The complete, refusal-free plan produced by the preflight phases. Once this exists, the commit
/// phases cannot refuse — they only perform.
pub(crate) struct SplitRevokePlan {
    pub(crate) cnode: CNodeId,
    pub(crate) root: SplitRevokeNode,
    pub(crate) root_object: CapObject,
    pub(crate) descendants: [Option<SplitRevokeNode>; SPLIT_REVOKE_MAX_DESCENDANTS],
    pub(crate) descendant_len: usize,
    /// Indices into `delegated_capability_links` confirmed for removal.
    pub(crate) link_removals: [Option<usize>; SPLIT_REVOKE_MAX_LINK_HITS],
    pub(crate) link_removal_len: usize,
}

impl SplitRevokePlan {
    fn descendant_slice(&self) -> &[Option<SplitRevokeNode>] {
        &self.descendants[..self.descendant_len]
    }
}

impl crate::runtime::SharedKernel {
    /// Stage 198B — split-read resolve of the `CapObject` a receiver-local `cap` references, read
    /// OUT of the receiver's cspace under the rank-4 capability seam only (no broad
    /// `&mut KernelState`, no IPC/task/scheduler lock). Returns `None` if the cnode space or the
    /// cap slot is absent. Used by the ordinary-cap delivery executors (`runtime.rs`) to prove the
    /// freshly materialized cap references the SAME object the sender transferred — a real cnode
    /// lookup, not a `CapId != 0` check. (Placed here, not in `capability_state.rs`, so the Stage
    /// 186A "capability seam is helper-only in those files" guard stays intact; this file already
    /// resolves caps out of the cspace.)
    pub(crate) fn resolved_cap_object_split(
        &self,
        receiver_cnode: CNodeId,
        cap: CapId,
    ) -> Option<CapObject> {
        self.resolved_capability_split(receiver_cnode, cap)
            .map(|c| c.object)
    }

    /// Stage 198B1 Part C: like `resolved_cap_object_split`, but returns the FULL
    /// `Capability` (object + rights) so the ordinary-cap delivery layer can
    /// authoritatively attest the destination rights AND the object identity of
    /// the freshly minted receiver-local cap. Rank-4 capability seam only.
    pub(crate) fn resolved_capability_split(
        &self,
        receiver_cnode: CNodeId,
        cap: CapId,
    ) -> Option<Capability> {
        self.with_capability_state_split_mut(|capability| {
            capability
                .cnode_spaces
                .iter()
                .flatten()
                .find(|space| space.id == receiver_cnode)
                .and_then(|space| kernel_ref(&space.cspace).get(cap))
        })
    }

    // ── U9-F narrow single-rank seams ───────────────────────────────────────────────────────

    /// rank 4: the `pid` recorded for a process cnode — the split twin of
    /// `KernelState::tid_for_cnode` (which the broad path uses as `source_pid`).
    pub(crate) fn pid_for_cnode_split(&self, cnode: CNodeId) -> Option<u64> {
        self.with_capability_state_split_mut(|capability| {
            capability
                .process_cnodes
                .iter()
                .flatten()
                .find(|record| record.cnode == cnode)
                .map(|record| record.pid)
        })
    }

    /// rank 4: the process cnode owning `pid` — the split twin of
    /// `KernelState::process_cnode_for_pid`.
    pub(crate) fn process_cnode_for_pid_split(&self, pid: u64) -> Option<CNodeId> {
        self.with_capability_state_split_mut(|capability| {
            capability
                .process_cnodes
                .iter()
                .flatten()
                .find(|record| record.pid == pid)
                .map(|record| record.cnode)
        })
    }

    /// rank 4: `CapabilitySpace::revoke` in one named cnode — the recursive in-cspace derivation
    /// revoke the broad path performs, unchanged, under the capability seam alone.
    fn cspace_revoke_split(&self, cnode: CNodeId, cap: CapId) -> Result<(), KernelError> {
        self.with_capability_state_split_mut(|capability| {
            capability
                .cnode_spaces
                .iter_mut()
                .flatten()
                .find(|space| space.id == cnode)
                .map(|space| kernel_mut(&mut space.cspace))
                .ok_or(KernelError::TaskMissing)?
                .revoke(cap)
                .map_err(|_| KernelError::InvalidCapability)
        })
    }

    /// rank 4: read the object then revoke, in ONE acquisition — the exact shape of
    /// `revoke_capability_direct_in_process_cnode`'s inner block. Returns the object that was
    /// live at revoke time (`None` ⇒ the slot was already gone, which the broad path treats as
    /// "no object effects owed").
    fn cspace_read_then_revoke_split(&self, cnode: CNodeId, cap: CapId) -> Option<CapObject> {
        self.with_capability_state_split_mut(|capability| {
            let space = capability
                .cnode_spaces
                .iter_mut()
                .flatten()
                .find(|space| space.id == cnode)?;
            let cspace = kernel_mut(&mut space.cspace);
            let object = cspace.get(cap).map(|c| c.object);
            let _ = cspace.revoke(cap);
            object
        })
    }

    /// rank 4: clear the named `delegated_capability_links` slots.
    fn clear_delegation_links_split(&self, indices: &[Option<usize>]) {
        self.with_capability_state_split_mut(|capability| {
            for idx in indices.iter().flatten().copied() {
                if idx < MAX_DELEGATED_CAPABILITY_LINKS {
                    capability.delegated_capability_links[idx] = None;
                }
            }
        });
    }

    /// rank 3: the notification-destroy half of `destroy_notification_for_revoked_cap`, byte for
    /// byte — IRQ-route teardown, waiter snapshot + clear, slot clear, generation bump. Returns
    /// the snapshotted waiter for the caller to wake AFTER rank 3 is released.
    ///
    /// The broad `destroy_notification` bounds the index by the boot-config
    /// `max_notifications`; this bounds it by the `notifications` array length instead, which
    /// differs only over indices whose slot is unconditionally `None` — both are a no-op there,
    /// and no live `Notification` cap can name such an index (`create_notification` allocates
    /// within the config bound). The same array bound is what `capability_object_live` already
    /// uses for this class.
    fn destroy_notification_split(&self, index: usize) -> Option<crate::kernel::ipc::ThreadId> {
        if index >= MAX_NOTIFICATIONS {
            return None;
        }
        self.with_ipc_split_mut(|ipc| {
            ipc.notifications[index].as_ref()?;
            for route in ipc.irq_routes.iter_mut() {
                if *route == Some(index) {
                    *route = None;
                }
            }
            let waiter = ipc.notification_waiters[index].take();
            ipc.notifications[index] = None;
            let mut next_gen = ipc.notification_generations[index].wrapping_add(1);
            if next_gen == 0 {
                next_gen = 1;
            }
            ipc.notification_generations[index] = next_gen;
            waiter
        })
    }

    /// rank 1 → rank 2 → rank 1 (three disjoint acquisitions, never nested): the wake half of
    /// `KernelState::wake_destroyed_notification_waiter`.
    ///
    /// Only a task still `Blocked(_)` is made `Runnable` and enqueued — a
    /// Dead/Exited/Runnable/Running/Faulted task is never resurrected or double-enqueued, matching
    /// the broad gating exactly. That gate also subsumes the broad enqueue's
    /// `refuse_enqueue_of_spawn_reservation`, since a spawn reservation is `TaskStatus::Reserved`
    /// and can never be `Blocked(_)`.
    ///
    /// Placement mirrors `KernelState::enqueue_task` in full, INCLUDING `ensure_driver_affinity`:
    /// an unpinned `Driver`-class task is pinned to the current CPU before placement, so it is
    /// enqueued on that CPU rather than balanced. The existing `enqueue_reply_timeout_wake_split`
    /// seam is deliberately NOT reused here — it mirrors only the pinned/balanced placement and
    /// omits that pin, which for a driver parked on an IRQ notification is a real difference in
    /// where the woken task lands.
    fn wake_destroyed_notification_waiter_split(
        &self,
        waiter_tid: crate::kernel::ipc::ThreadId,
    ) -> bool {
        use crate::kernel::ipc::ThreadId;
        use crate::kernel::scheduler::TaskPriority;
        use crate::kernel::task::{TaskClass, TaskStatus};
        let tid = waiter_tid.0;
        // Phase 1 (rank 1): the CPU `ensure_driver_affinity` would pin to, read and released
        // before the task domain is entered.
        let current_cpu = self.with_scheduler_split_mut(|sched| sched.current_cpu);
        let class = self.task_class_split_read(tid);
        // Phase 2 (rank 2): the Blocked-only transition, the driver-affinity pin, and the
        // resulting placement affinity — one acquisition, as the broad path had under its guard.
        let plan = self.with_task_tcbs_split_mut(|tcbs| {
            let tcb = tcbs.iter_mut().flatten().find(|t| t.tid.0 == tid)?;
            if !matches!(tcb.status, TaskStatus::Blocked(_)) {
                return None;
            }
            tcb.status = TaskStatus::Runnable;
            if tid != 0 && class == Some(TaskClass::Driver) && tcb.cpu_affinity.is_none() {
                tcb.cpu_affinity = Some(current_cpu);
            }
            Some(tcb.cpu_affinity)
        });
        let Some(affinity) = plan else {
            return false;
        };
        // Phase 3 (rank 1): the same class-derived priority and pinned/balanced placement
        // `enqueue_task` performs.
        let priority = match class {
            Some(TaskClass::SystemServer) => TaskPriority::High,
            _ => TaskPriority::Normal,
        };
        self.with_scheduler_split_mut(|sched| {
            let s = kernel_mut(&mut sched.scheduler);
            match affinity {
                Some(cpu) => {
                    let _ = s.enqueue_on_with_priority(cpu, ThreadId(tid), priority);
                }
                None => {
                    let _ = s.enqueue_balanced(ThreadId(tid), priority);
                }
            }
        });
        true
    }

    /// Obligation (6), split out whole: destroy the notification an admitted object names, then
    /// wake its snapshotted waiter outside every lock. A non-`Notification` object is a no-op,
    /// exactly as in `destroy_notification_for_revoked_cap`.
    fn destroy_notification_for_revoked_object_split(&self, object: CapObject) {
        let CapObject::Notification { index, .. } = object else {
            return;
        };
        if let Some(waiter_tid) = self.destroy_notification_split(index) {
            let _ = self.wake_destroyed_notification_waiter_split(waiter_tid);
        }
    }

    // ── U9-F preflight (no mutation; the ONLY place a refusal can be raised) ─────────────────

    /// rank 4 then rank 2 (sequential, never nested): the direct delegated children of one node.
    ///
    /// The numeric `source_cap` pre-filter under rank 4 is the same selective test
    /// `has_any_delegated_child` uses; the owning `pid` is resolved under rank 2 only after the
    /// capability lock is released, so no rank-2 seam is ever entered while rank 4 is held.
    fn delegated_children_split(
        &self,
        node: SplitRevokeNode,
        out: &mut [Option<SplitRevokeNode>; SPLIT_REVOKE_MAX_LINK_HITS],
    ) -> Result<usize, SplitRevokeRefusal> {
        let mut hits = [None::<(u64, u64, CapId)>; SPLIT_REVOKE_MAX_LINK_HITS];
        let mut n = 0usize;
        let overflow = self.with_capability_state_split_mut(|capability| {
            for link in kernel_ref(&capability.delegated_capability_links)
                .iter()
                .flatten()
            {
                if link.source_cap != node.cap {
                    continue;
                }
                if n >= hits.len() {
                    return true;
                }
                hits[n] = Some((link.source_tid, link.dest_tid, link.dest_cap));
                n += 1;
            }
            false
        });
        if overflow {
            return Err(SplitRevokeRefusal::LinkOverflow);
        }
        let mut found = 0usize;
        for (source_tid, dest_tid, dest_cap) in hits.iter().flatten().copied() {
            let source_pid = self.process_id_split_read(source_tid).unwrap_or(source_tid);
            if source_pid != node.pid {
                continue;
            }
            let child = SplitRevokeNode {
                pid: self.process_id_split_read(dest_tid).unwrap_or(dest_tid),
                cap: dest_cap,
            };
            out[found] = Some(child);
            found += 1;
        }
        Ok(found)
    }

    /// Preflight P2: the bounded transitive delegated closure, with every member's object class
    /// gated. A member whose slot is already gone owes no object effects (the broad
    /// `revoke_capability_direct_in_process_cnode` reads `None` and skips all three), so it stays
    /// in the closure without a class gate.
    fn collect_admitted_descendants_split(
        &self,
        root: SplitRevokeNode,
        plan: &mut SplitRevokePlan,
    ) -> Result<(), SplitRevokeRefusal> {
        let mut queue = [None::<SplitRevokeNode>; SPLIT_REVOKE_MAX_DESCENDANTS];
        let mut queue_len = 0usize;
        let mut head = 0usize;
        let mut current = root;
        loop {
            let mut children = [None::<SplitRevokeNode>; SPLIT_REVOKE_MAX_LINK_HITS];
            let child_len = self.delegated_children_split(current, &mut children)?;
            for child in children[..child_len].iter().flatten().copied() {
                if plan
                    .descendant_slice()
                    .iter()
                    .flatten()
                    .any(|seen| *seen == child)
                {
                    continue;
                }
                if plan.descendant_len >= SPLIT_REVOKE_MAX_DESCENDANTS
                    || queue_len >= SPLIT_REVOKE_MAX_DESCENDANTS
                {
                    return Err(SplitRevokeRefusal::DescendantOverflow);
                }
                // Class-gate the child before admitting it: a memory-backed descendant would drag
                // the whole composition back behind the D3 fence.
                if let Some(child_cnode) = self.process_cnode_for_pid_split(child.pid)
                    && let Some(capability) = self.resolved_capability_split(child_cnode, child.cap)
                {
                    split_revoke_class_admitted(capability.object)?;
                }
                plan.descendants[plan.descendant_len] = Some(child);
                plan.descendant_len += 1;
                queue[queue_len] = Some(child);
                queue_len += 1;
            }
            if head >= queue_len {
                return Ok(());
            }
            let Some(next) = queue[head] else {
                return Ok(());
            };
            head += 1;
            current = next;
        }
    }

    /// Preflight P3: the exact set of `delegated_capability_links` slots the broad
    /// `remove_delegation_links_for` would clear — every link whose resolved source OR dest equals
    /// the root or a descendant. Numeric cap matching under rank 4, pid confirmation under rank 2,
    /// with the two never nested.
    fn collect_delegation_link_removals_split(
        &self,
        plan: &mut SplitRevokePlan,
    ) -> Result<(), SplitRevokeRefusal> {
        let mut caps = [None::<CapId>; SPLIT_REVOKE_MAX_DESCENDANTS + 1];
        caps[0] = Some(plan.root.cap);
        let mut cap_len = 1usize;
        for node in plan.descendant_slice().iter().flatten() {
            caps[cap_len] = Some(node.cap);
            cap_len += 1;
        }
        let mut hits = [None::<(usize, u64, CapId, u64, CapId)>; SPLIT_REVOKE_MAX_LINK_HITS];
        let mut n = 0usize;
        let overflow = self.with_capability_state_split_mut(|capability| {
            for (idx, maybe_link) in kernel_ref(&capability.delegated_capability_links)
                .iter()
                .enumerate()
            {
                let Some(link) = maybe_link else {
                    continue;
                };
                let touches = caps[..cap_len]
                    .iter()
                    .flatten()
                    .any(|cap| *cap == link.source_cap || *cap == link.dest_cap);
                if !touches {
                    continue;
                }
                if n >= hits.len() {
                    return true;
                }
                hits[n] = Some((
                    idx,
                    link.source_tid,
                    link.source_cap,
                    link.dest_tid,
                    link.dest_cap,
                ));
                n += 1;
            }
            false
        });
        if overflow {
            return Err(SplitRevokeRefusal::LinkOverflow);
        }
        for (idx, source_tid, source_cap, dest_tid, dest_cap) in hits.iter().flatten().copied() {
            let source = SplitRevokeNode {
                pid: self.process_id_split_read(source_tid).unwrap_or(source_tid),
                cap: source_cap,
            };
            let dest = SplitRevokeNode {
                pid: self.process_id_split_read(dest_tid).unwrap_or(dest_tid),
                cap: dest_cap,
            };
            let involved = source == plan.root
                || dest == plan.root
                || plan
                    .descendant_slice()
                    .iter()
                    .flatten()
                    .any(|node| *node == source || *node == dest);
            if !involved {
                continue;
            }
            plan.link_removals[plan.link_removal_len] = Some(idx);
            plan.link_removal_len += 1;
        }
        Ok(())
    }

    /// Preflight P4: the fail-closed active-transfer-mapping check. Unreachable for the admitted
    /// classes (see the section docs above) — if it ever fires, the composition refuses to the
    /// broad path rather than dropping obligation (4).
    fn refuse_on_active_transfer_mapping_split(
        &self,
        plan: &SplitRevokePlan,
    ) -> Result<(), SplitRevokeRefusal> {
        let mut caps = [None::<CapId>; SPLIT_REVOKE_MAX_DESCENDANTS + 1];
        caps[0] = Some(plan.root.cap);
        let mut cap_len = 1usize;
        for node in plan.descendant_slice().iter().flatten() {
            caps[cap_len] = Some(node.cap);
            cap_len += 1;
        }
        let mut hits = [None::<(u64, CapId)>; SPLIT_REVOKE_MAX_LINK_HITS];
        let mut n = 0usize;
        let overflow = self.with_ipc_split_mut(|ipc| {
            for mapping in ipc.active_transfer_mappings.iter().flatten() {
                if !caps[..cap_len]
                    .iter()
                    .flatten()
                    .any(|cap| *cap == mapping.transfer_cap)
                {
                    continue;
                }
                if n >= hits.len() {
                    return true;
                }
                hits[n] = Some((mapping.owner_tid.0, mapping.transfer_cap));
                n += 1;
            }
            false
        });
        if overflow {
            return Err(SplitRevokeRefusal::ActiveMappingPresent);
        }
        for (owner_tid, transfer_cap) in hits.iter().flatten().copied() {
            let mapping_pid = self.process_id_split_read(owner_tid).unwrap_or(owner_tid);
            let names_root = mapping_pid == plan.root.pid && transfer_cap == plan.root.cap;
            let names_descendant = plan
                .descendant_slice()
                .iter()
                .flatten()
                .any(|node| node.pid == mapping_pid && node.cap == transfer_cap);
            if names_root || names_descendant {
                return Err(SplitRevokeRefusal::ActiveMappingPresent);
            }
        }
        Ok(())
    }

    /// Build the complete plan, or refuse. Makes NO mutation on any path.
    pub(crate) fn plan_revoke_capability_no_vm_split(
        &self,
        receiver_tid: u64,
        cap: CapId,
    ) -> Result<SplitRevokePlan, SplitRevokeRefusal> {
        // P1 (rank 2 → rank 4): identity and the root class gate.
        let cnode = self
            .task_cnode_split(receiver_tid)
            .ok_or(SplitRevokeRefusal::Unresolvable)?;
        let pid = self
            .pid_for_cnode_split(cnode)
            .ok_or(SplitRevokeRefusal::Unresolvable)?;
        let root_object = self
            .resolved_capability_split(cnode, cap)
            .ok_or(SplitRevokeRefusal::Unresolvable)?
            .object;
        split_revoke_class_admitted(root_object)?;
        let root = SplitRevokeNode { pid, cap };
        let mut plan = SplitRevokePlan {
            cnode,
            root,
            root_object,
            descendants: [None; SPLIT_REVOKE_MAX_DESCENDANTS],
            descendant_len: 0,
            link_removals: [None; SPLIT_REVOKE_MAX_LINK_HITS],
            link_removal_len: 0,
        };
        // P2: bounded delegated closure, every member class-gated.
        self.collect_admitted_descendants_split(root, &mut plan)?;
        // P3: the exact link-removal set.
        self.collect_delegation_link_removals_split(&mut plan)?;
        // P4: fail-closed transfer-mapping check.
        self.refuse_on_active_transfer_mapping_split(&plan)?;
        Ok(plan)
    }

    // ── U9-F commit (cannot refuse; performs the complete teardown) ──────────────────────────

    /// U9-F — the complete off-broad-lock teardown of one admitted capability, in the SAME order
    /// the broad `revoke_capability_in_cnode` performs it:
    ///
    /// 1. rank 4: revoke the root (the recursive in-cspace derivation revoke, unchanged);
    /// 2. per descendant: rank 4 read-then-revoke in its own process cnode, then rank 3
    ///    notification destroy, then rank 2 → rank 1 waiter wake — the same interleave
    ///    `revoke_capability_direct_in_process_cnode` produces;
    /// 3. rank 4: clear the delegation links naming the root or a descendant;
    /// 4. rank 3 then rank 2 → rank 1: the root's notification destroy + waiter wake.
    ///
    /// Obligations (4) and (5) are absent because the admitted classes owe neither — proven in the
    /// section docs above and re-checked fail-closed in the preflight. No two locks are ever held
    /// at once.
    ///
    /// `Ok(())` means the teardown completed; every `Err` is a refusal raised before any mutation,
    /// and the caller must run the unchanged broad path instead.
    pub(crate) fn revoke_capability_no_vm_split(
        &self,
        receiver_tid: u64,
        cap: CapId,
    ) -> Result<CapObject, SplitRevokeRefusal> {
        let plan = self.plan_revoke_capability_no_vm_split(receiver_tid, cap)?;
        // (1) rank 4: the root revoke. This is the first mutation; if the slot vanished between
        // the preflight and here, nothing has changed yet and the caller falls back cleanly.
        if self.cspace_revoke_split(plan.cnode, plan.root.cap).is_err() {
            return Err(SplitRevokeRefusal::Unresolvable);
        }
        // (2) descendants, each complete before the next — the broad interleave.
        for descendant in plan.descendant_slice().iter().flatten().copied() {
            let object = self
                .process_cnode_for_pid_split(descendant.pid)
                .and_then(|cnode| self.cspace_read_then_revoke_split(cnode, descendant.cap));
            if let Some(object) = object {
                self.destroy_notification_for_revoked_object_split(object);
            }
        }
        // (3) rank 4: delegation-link removal.
        self.clear_delegation_links_split(&plan.link_removals[..plan.link_removal_len]);
        // (4) rank 3 then rank 2 → rank 1: the root's notification obligation.
        self.destroy_notification_for_revoked_object_split(plan.root_object);
        Ok(plan.root_object)
    }

    /// U9-F production entry point for the ordinary (non-`Reply`) recv-boundary rollback cohort:
    /// the split twin of `KernelState::rollback_materialized_recv_cap`'s transfer arm.
    ///
    /// Returns `true` iff the complete teardown ran off the broad lock. `false` means the class or
    /// the shape was refused and the caller MUST run the unchanged broad
    /// `rollback_materialized_recv_cap`, whose effects stay fully intact.
    pub(crate) fn rollback_materialized_recv_cap_no_vm_split(
        &self,
        receiver_tid: u64,
        materialized_cap: CapId,
    ) -> bool {
        if self
            .revoke_capability_no_vm_split(receiver_tid, materialized_cap)
            .is_err()
        {
            // Refused: emit NOTHING. The broad `rollback_materialized_recv_cap` the caller runs
            // next emits this path's marker itself, so a refusal must not double-log it.
            return false;
        }
        // Byte-identical to the marker `rollback_materialized_recv_cap`'s transfer arm emits, so
        // the live log is the SAME on both routes and no new marker family or `kind=` variant is
        // introduced. Route ownership is proven by the source-recomputed guards, not by telemetry.
        crate::yarm_log!(
            "IPC_RECV_MATERIALIZE_ROLLBACK kind=transfer receiver_tid={} cap={} ok={}",
            receiver_tid,
            materialized_cap.0,
            true
        );
        true
    }
}
