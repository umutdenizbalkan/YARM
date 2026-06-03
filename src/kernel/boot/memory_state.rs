// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState, MemoryObject, MemoryObjectKind, kernel_mut};
use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
use crate::kernel::frame_allocator::FrameAllocError;
use crate::kernel::scheduler::CpuId;
use crate::kernel::topology::CpuBitmap;
use crate::kernel::vm::{Asid, Mapping, MappingEntry, PageFlags, PhysAddr, VirtAddr, VmError};

impl KernelState {
    fn begin_live_tlb_shootdown_wait(&mut self, requester: CpuId, targets: CpuBitmap) -> u64 {
        self.with_ipc_state_mut(|ipc| {
            let sequence = ipc.live_tlb_shootdown.next_sequence;
            ipc.live_tlb_shootdown.next_sequence =
                ipc.live_tlb_shootdown.next_sequence.wrapping_add(1);
            if ipc.live_tlb_shootdown.next_sequence == 0 {
                ipc.live_tlb_shootdown.next_sequence = 1;
            }
            ipc.live_tlb_shootdown.active = Some(super::LiveTlbShootdownWait {
                sequence,
                pending_cpu_bitmap: targets,
                requester_cpu: requester,
            });
            sequence
        })
    }

    fn live_tlb_shootdown_pending(&self) -> u64 {
        self.with_ipc_state(|ipc| {
            ipc.live_tlb_shootdown
                .active
                .map(|wait| wait.pending_cpu_bitmap)
                .unwrap_or(0)
        })
    }

    fn clear_live_tlb_shootdown_wait(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.live_tlb_shootdown.active = None;
        });
    }

    fn mark_cow_page(&mut self, asid: Asid, virt: VirtAddr) -> Result<(), KernelError> {
        self.with_memory_state_mut(|memory| {
            #[cfg(test)]
            if let Some(limit) = memory.cow_page_capacity_limit {
                let total: usize = memory.cow_pages.values().map(|s| s.len()).sum();
                if total >= limit {
                    return Err(KernelError::MemoryObjectFull);
                }
            }
            memory
                .cow_pages
                .entry(asid.0)
                .or_insert_with(alloc::collections::BTreeSet::new)
                .insert(virt.0);
            Ok(())
        })
    }

    fn clear_cow_page(&mut self, asid: Asid, virt: VirtAddr) {
        self.with_memory_state_mut(|memory| {
            if let Some(set) = memory.cow_pages.get_mut(&asid.0) {
                set.remove(&virt.0);
                if set.is_empty() {
                    memory.cow_pages.remove(&asid.0);
                }
            }
        });
    }

    fn clear_cow_pages_for_asid(&mut self, asid: Asid) {
        self.with_memory_state_mut(|memory| {
            memory.cow_pages.remove(&asid.0);
        });
    }

    pub(crate) fn is_cow_page(&self, asid: Asid, virt: VirtAddr) -> bool {
        self.with_memory_state(|memory| {
            memory
                .cow_pages
                .get(&asid.0)
                .map_or(false, |set| set.contains(&virt.0))
        })
    }

    #[cfg(test)]
    pub(crate) fn cow_page_count(&self) -> usize {
        self.with_memory_state(|memory| memory.cow_pages.values().map(|s| s.len()).sum())
    }

    #[cfg(test)]
    pub(crate) fn cow_page_count_for_asid(&self, asid: Asid) -> usize {
        self.with_memory_state(|memory| {
            memory.cow_pages.get(&asid.0).map_or(0, |s| s.len())
        })
    }

    #[cfg(test)]
    pub(crate) fn cow_asid_bucket_count(&self) -> usize {
        self.with_memory_state(|memory| memory.cow_pages.len())
    }

    pub fn destroy_user_address_space(&mut self, aspace_cap: CapId) -> Result<(), KernelError> {
        let cnode = self.current_task_cnode().ok_or(KernelError::TaskMissing)?;
        let capability = self
            .capability_for_cnode_local(cnode, aspace_cap)
            .ok_or(KernelError::InvalidCapability)?;
        let asid = match capability.object {
            CapObject::AddressSpace { asid } => Asid(asid),
            _ => return Err(KernelError::WrongObject),
        };
        if !capability.has_right(CapRights::MAP) {
            return Err(KernelError::MissingRight);
        }

        self.revoke_capability_in_cnode(cnode, aspace_cap)?;

        self.destroy_user_address_space_by_asid(asid)
    }

    pub fn create_user_address_space(&mut self) -> Result<(Asid, CapId), KernelError> {
        let asid = self
            .with_user_spaces_mut(|spaces| spaces.create_user_space())
            .map_err(KernelError::Vm)?;
        let map_cap = self.mint_capability_for_current_context(Capability::new(
            CapObject::AddressSpace { asid: asid.0 },
            CapRights::MAP | CapRights::READ | CapRights::WRITE,
        ))?;
        Ok((asid, map_cap))
    }

    fn live_cpu_bitmap_for_asid(&self, asid: Asid) -> CpuBitmap {
        let online = self.online_cpu_bitmap();
        let mut bitmap: CpuBitmap = 0;
        for cpu in 0..u64::BITS as usize {
            let cpu_bit = 1u64 << cpu;
            if (online & cpu_bit) == 0 {
                continue;
            }
            let cpu_id = CpuId(cpu as u8);
            if self
                .current_tid_on_cpu(cpu_id)
                .and_then(|tid| self.task_asid(tid))
                == Some(asid)
            {
                bitmap |= cpu_bit;
            }
        }
        bitmap
    }

    /// Stage 5D: Compute the TLB shootdown target set for a single-page unmap
    /// without acquiring any vm or ipc domain locks.
    ///
    /// Reads scheduler (rank 1) + task (rank 2) state to determine which CPUs
    /// are currently running the given ASID, then returns a `TlbShootdownRequestPlan`
    /// that captures the target bitmap and requester CPU. The caller can use this
    /// snapshot to avoid recomputing the bitmap on every iteration of an unmap loop.
    ///
    /// When `plan.target_cpu_bitmap == 0`, no cross-CPU notification is needed and
    /// `request_live_asid_shootdown` returns immediately — the unmap is ipc-lock-free.
    pub(crate) fn compute_tlb_shootdown_request_plan(
        &self,
        asid: Asid,
        virt: VirtAddr,
    ) -> super::TlbShootdownRequestPlan {
        let requester = self.current_cpu();
        let requester_bit = 1u64 << requester.0;
        let target_cpu_bitmap = self.live_cpu_bitmap_for_asid(asid) & !requester_bit;
        super::TlbShootdownRequestPlan {
            asid,
            virt,
            target_cpu_bitmap,
            requester,
        }
    }

    /// Stage 5F (Part 1): rank-ordering characterisation for Stage 5E blocker #1a.
    ///
    /// ## Lock/rank ordering
    ///
    /// This function acquires the ipc domain lock (rank 3) to register the
    /// pending shootdown. The critical ordering property is:
    ///
    ///   **All vm (rank 5) and memory (rank 6) locks are RELEASED before this
    ///   function is called. There is no simultaneous vm/memory/ipc nesting.**
    ///
    /// The correct call sequence in the two-phase design is:
    ///
    /// ```text
    ///   Phase 1 — vm(5) acquired → page removed → vm(5) released
    ///             memory(6) acquired → clear COW, decrement refcount → memory(6) released
    ///   Phase 2 — ipc(3) acquired [this function] → shootdown wait → ipc(3) released
    ///   Phase 3 — memory(6) acquired → reclaim frame → memory(6) released
    /// ```
    ///
    /// ## Real hazard (Stage 5E blocker #1b)
    ///
    /// The hazard is NOT that ipc(3) and vm(5)/memory(6) are held simultaneously —
    /// they are never simultaneously held. The hazard is that the old callers
    /// (`unmap_user_page_in_*`) call `reclaim_memory_object_for_phys` BEFORE
    /// this function. Under the global lock that ordering is safe; for global-lock
    /// removal, the frame must not be freed until after shootdown completes.
    /// The `unmap_page_phase1` + `execute_tlb_shootdown_wait_plan` pattern fixes
    /// this ordering.
    ///
    /// ## Fast path
    ///
    /// If `targets == 0` (no remote CPU has the ASID loaded), this function
    /// returns immediately without acquiring the ipc lock. TLB shootdown is not
    /// needed. In single-CPU (hosted-dev), this is always the path taken.
    fn request_live_asid_shootdown(
        &mut self,
        asid: Asid,
        virt: VirtAddr,
    ) -> Result<(), KernelError> {
        let requester = self.current_cpu();
        let requester_bit = 1u64 << requester.0;
        let targets = self.live_cpu_bitmap_for_asid(asid) & !requester_bit;
        if targets == 0 {
            return Ok(());
        }
        let sequence = self.begin_live_tlb_shootdown_wait(requester, targets);
        // Ordering note: mapping removal completes before we publish shootdown
        // work items, so remote CPUs can only ACK after invalidating post-unmap
        // state.
        for cpu in 0..u64::BITS as usize {
            let cpu_bit = 1u64 << cpu;
            if (targets & cpu_bit) == 0 {
                continue;
            }
            self.submit_cross_cpu_work(
                CpuId(cpu as u8),
                crate::kernel::smp::WorkItem::TlbShootdown {
                    asid,
                    va_range: Some((virt, virt + crate::kernel::vm::PAGE_SIZE as u64)),
                    requester: Some(requester),
                    sequence,
                },
            )?;
        }
        while self.live_tlb_shootdown_pending() != 0 {
            let pending_before = self.live_tlb_shootdown_pending();
            for cpu in 0..u64::BITS as usize {
                let cpu_bit = 1u64 << cpu;
                if (targets & cpu_bit) == 0 {
                    continue;
                }
                let remote = CpuId(cpu as u8);
                let previous = self.current_cpu();
                self.set_current_cpu(remote)?;
                let _ = self.process_cross_cpu_work_for_cpu(remote)?;
                self.set_current_cpu(previous)?;
            }
            let _ = self.process_cross_cpu_work_for_cpu(requester)?;
            if self.live_tlb_shootdown_pending() == pending_before {
                // Avoid pure tight spinning while waiting for remote mailbox
                // progress; this keeps the wait path scheduler-friendly.
                self.yield_current()?;
            }
        }
        self.clear_live_tlb_shootdown_wait();
        Ok(())
    }

    pub(crate) fn destroy_user_address_space_by_asid(
        &mut self,
        asid: Asid,
    ) -> Result<(), KernelError> {
        self.clear_cow_pages_for_asid(asid);
        let pending_cpu_bitmap = self.online_cpu_bitmap();
        let drained = self
            .with_user_spaces_mut(|spaces| {
                spaces.destroy_and_collect_mappings(asid, pending_cpu_bitmap)
            })
            .map_err(KernelError::Vm)?;

        // Stage 18 ordering fix: submit TLB shootdown work items BEFORE
        // reclaiming frames.  Under the global lock both orderings are safe,
        // but this matches the two-phase-unmap contract (shootdown precedes
        // reclaim) and is the correct direction for future lock-free SMP.
        //
        // Shootdown is fire-and-forget (requester: None, sequence: 0).
        // Queue-full errors are silenced: the ASID is already retired and
        // frames must be reclaimed regardless.  A full queue means other
        // work is pending; the TLB will eventually be invalidated when that
        // work drains.  Frame reuse before invalidation cannot happen because
        // the retired ASID cannot be reused until all CPUs ACK it.
        for cpu in 0..u64::BITS as usize {
            let cpu_bit = 1u64 << cpu;
            if (pending_cpu_bitmap & cpu_bit) == 0 {
                continue;
            }
            let _ = self.submit_cross_cpu_work(
                crate::kernel::scheduler::CpuId(cpu as u8),
                crate::kernel::smp::WorkItem::TlbShootdown {
                    asid,
                    va_range: None,
                    requester: None,
                    sequence: 0,
                },
            );
        }

        // Reclaim physical frames after shootdown work items have been queued.
        for mapping in drained.into_iter().flatten() {
            self.note_mapping_removed(mapping.phys);
            self.reclaim_memory_object_for_phys(mapping.phys);
        }

        Ok(())
    }

    pub(crate) fn clone_user_address_space_cow(
        &mut self,
        parent_asid: Asid,
    ) -> Result<Asid, KernelError> {
        if self.with_user_spaces(|spaces| spaces.get(parent_asid).is_none()) {
            return Err(KernelError::Vm(VmError::InvalidAsid));
        }
        let child_asid = self
            .with_user_spaces_mut(|spaces| spaces.create_user_space())
            .map_err(KernelError::Vm)?;

        // Track parent pages we write-protected during the clone.  On any
        // failure we must restore their write permission so the parent process
        // can still write to those pages.  Without restoration a write would
        // produce an unhandled fault (no COW record, no demand-page entry).
        let mut wp_parent_virts: alloc::vec::Vec<VirtAddr> = alloc::vec::Vec::new();

        let mut index = 0usize;
        loop {
            let maybe_entry = self.with_user_spaces(|spaces| {
                spaces
                    .get(parent_asid)
                    .and_then(|aspace| aspace.mapping_at(index))
            });
            let Some(MappingEntry { virt, mapping }) = maybe_entry else {
                break;
            };
            let mut shared_flags = mapping.flags;
            if mapping.flags.write {
                shared_flags.write = false;
            }
            if let Err(err) = self.map_user_page_in_asid_raw(
                child_asid,
                virt,
                Mapping {
                    phys: mapping.phys,
                    flags: shared_flags,
                },
            ) {
                let _ = self.destroy_user_address_space_by_asid(child_asid);
                self.restore_parent_write_permissions(parent_asid, &wp_parent_virts);
                return Err(err);
            }
            if mapping.flags.write {
                if let Err(err) = self.map_user_page_in_asid_raw(
                    parent_asid,
                    virt,
                    Mapping {
                        phys: mapping.phys,
                        flags: shared_flags,
                    },
                ) {
                    let _ = self.destroy_user_address_space_by_asid(child_asid);
                    self.restore_parent_write_permissions(parent_asid, &wp_parent_virts);
                    return Err(err);
                }
                // Record the write-protected page before COW marking so the
                // rollback covers it even if mark_cow_page(parent) fails.
                wp_parent_virts.push(virt);
                if let Err(err) = self.mark_cow_page(parent_asid, virt) {
                    let _ = self.destroy_user_address_space_by_asid(child_asid);
                    self.restore_parent_write_permissions(parent_asid, &wp_parent_virts);
                    return Err(err);
                }
                if let Err(err) = self.mark_cow_page(child_asid, virt) {
                    let _ = self.destroy_user_address_space_by_asid(child_asid);
                    self.restore_parent_write_permissions(parent_asid, &wp_parent_virts);
                    return Err(err);
                }
            }
            #[cfg(feature = "hosted-dev")]
            self.with_memory_state_mut(|memory| {
                for offset in 0..crate::kernel::vm::PAGE_SIZE as u64 {
                    let from = (parent_asid.0, mapping.phys.0 + offset);
                    let to = (child_asid.0, mapping.phys.0 + offset);
                    if let Some(value) = memory.user_memory.get(&from).copied() {
                        memory.user_memory.insert(to, value);
                    }
                }
            });
            index += 1;
        }

        Ok(child_asid)
    }

    /// Restore write permission for each parent page that was write-protected
    /// during a failed `clone_user_address_space_cow`.
    ///
    /// Calling `map_user_page_in_asid_raw` with `flags.write=true` both
    /// restores the PTE write bit and clears any COW record that was added for
    /// the page, leaving `map_refcount` and `cap_refcount` unchanged (net-zero
    /// note_mapping_removed / note_mapping_inserted pair on the same frame).
    fn restore_parent_write_permissions(
        &mut self,
        parent_asid: Asid,
        wp_virts: &[VirtAddr],
    ) {
        for &virt in wp_virts {
            let current = self.with_user_spaces(|spaces| {
                spaces.get(parent_asid).and_then(|a| a.resolve(virt))
            });
            if let Some(mapping) = current {
                let mut restored = mapping.flags;
                restored.write = true;
                let _ = self.map_user_page_in_asid_raw(
                    parent_asid,
                    virt,
                    Mapping {
                        phys: mapping.phys,
                        flags: restored,
                    },
                );
            }
        }
    }

    fn copy_frame_contents_for_cow(
        &mut self,
        asid: Asid,
        old_phys: PhysAddr,
        new_phys: PhysAddr,
    ) -> Result<(), KernelError> {
        #[cfg(feature = "hosted-dev")]
        {
            self.with_memory_state_mut(|memory| {
                for offset in 0..crate::kernel::vm::PAGE_SIZE as u64 {
                    let key = (asid.0, old_phys.0 + offset);
                    if let Some(value) = memory.user_memory.get(&key).copied() {
                        memory.user_memory.insert((asid.0, new_phys.0 + offset), value);
                    }
                }
            });
            Ok(())
        }
        #[cfg(not(feature = "hosted-dev"))]
        {
            let _ = asid;
            let src =
                Self::phys_to_direct_map_ptr(old_phys.0).ok_or(KernelError::UserMemoryFault)?;
            let dst =
                Self::phys_to_direct_map_ptr(new_phys.0).ok_or(KernelError::UserMemoryFault)?;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    src as *const u8,
                    dst,
                    crate::kernel::vm::PAGE_SIZE,
                );
            }
            Ok(())
        }
    }

    pub(crate) fn try_handle_cow_fault(
        &mut self,
        asid: Asid,
        fault_addr: VirtAddr,
    ) -> Result<bool, KernelError> {
        let page = fault_addr.page_align_down();
        if !self.is_cow_page(asid, page) {
            return Ok(false);
        }
        let mapping = self
            .with_user_spaces(|spaces| spaces.get(asid).and_then(|aspace| aspace.resolve(page)))
            .ok_or(KernelError::UserMemoryFault)?;
        if mapping.flags.write {
            self.clear_cow_page(asid, page);
            return Ok(true);
        }
        let (_id, new_mem_cap) = self.alloc_anonymous_memory_object()?;
        let new_phys = match self.resolve_memory_object_phys(new_mem_cap, PageFlags::USER_RW) {
            Ok(p) => p,
            Err(e) => {
                if let Some(cnode) = self.current_task_cnode() {
                    let _ = self.revoke_capability_in_cnode(cnode, new_mem_cap);
                }
                return Err(e);
            }
        };
        if let Err(e) = self.copy_frame_contents_for_cow(asid, mapping.phys, new_phys) {
            if let Some(cnode) = self.current_task_cnode() {
                let _ = self.revoke_capability_in_cnode(cnode, new_mem_cap);
            }
            return Err(e);
        }
        let mut flags = mapping.flags;
        flags.write = true;
        if let Err(e) =
            self.map_user_page_in_asid_raw(asid, page, Mapping { phys: new_phys, flags })
        {
            if let Some(cnode) = self.current_task_cnode() {
                let _ = self.revoke_capability_in_cnode(cnode, new_mem_cap);
            }
            return Err(e);
        }
        self.clear_cow_page(asid, page);
        Ok(true)
    }

    pub fn map_user_page(
        &mut self,
        map_cap: CapId,
        virt: VirtAddr,
        mapping: Mapping,
    ) -> Result<Option<Mapping>, KernelError> {
        let cnode = self.current_task_cnode().ok_or(KernelError::TaskMissing)?;
        let capability = self
            .capability_for_cnode_local(cnode, map_cap)
            .ok_or(KernelError::InvalidCapability)?;
        let asid = match capability.object {
            CapObject::AddressSpace { asid } => Asid(asid),
            _ => return Err(KernelError::WrongObject),
        };
        if !capability.has_right(CapRights::MAP) {
            return Err(KernelError::MissingRight);
        }

        self.map_user_page_in_asid_raw(asid, virt, mapping)
    }

    pub fn create_memory_object(&mut self, phys: PhysAddr) -> Result<(u64, CapId), KernelError> {
        if !phys.0.is_multiple_of(crate::kernel::vm::PAGE_SIZE as u64) {
            return Err(KernelError::Vm(VmError::Misaligned));
        }
        self.create_memory_object_with_len(phys, crate::kernel::vm::PAGE_SIZE)
    }

    fn create_memory_object_with_len(
        &mut self,
        phys: PhysAddr,
        len: usize,
    ) -> Result<(u64, CapId), KernelError> {
        self.create_memory_object_with_len_and_kind(phys, len, MemoryObjectKind::Anonymous)
    }

    fn create_memory_object_with_len_and_kind(
        &mut self,
        phys: PhysAddr,
        len: usize,
        kind: MemoryObjectKind,
    ) -> Result<(u64, CapId), KernelError> {
        if len == 0 || !len.is_multiple_of(crate::kernel::vm::PAGE_SIZE) {
            return Err(KernelError::Vm(VmError::Misaligned));
        }
        if self.with_memory_state(|memory| memory.memory_objects.iter().flatten().count())
            >= self.runtime_capacity_config().max_memory_objects
        {
            return Err(KernelError::MemoryObjectFull);
        }
        let id = self.with_memory_state_mut(|memory| {
            let id = memory.next_memory_object_id;
            memory.next_memory_object_id = memory.next_memory_object_id.wrapping_add(1);
            let slot = memory
                .memory_objects
                .iter_mut()
                .find(|entry| entry.is_none())
                .ok_or(KernelError::MemoryObjectFull)?;
            *slot = Some(MemoryObject {
                id,
                phys,
                len,
                cap_refcount: 0,
                map_refcount: 0,
                pin_refcount: 0,
                kind,
            });
            Ok::<u64, KernelError>(id)
        })?;

        let rights = match kind {
            MemoryObjectKind::Anonymous => CapRights::READ | CapRights::WRITE | CapRights::MAP,
            // File-backed slices are read-only: no WRITE right.
            MemoryObjectKind::InitramfsFileSlice { .. } => CapRights::READ | CapRights::MAP,
        };
        let cap = self.mint_capability_for_current_context(Capability::new(
            CapObject::MemoryObject { id },
            rights,
        ))?;

        Ok((id, cap))
    }

    /// Create a read-only `MemoryObject` backed by a slice of the boot initramfs CPIO.
    ///
    /// `initrd` is the full initrd byte slice (from `boot_initrd_bytes()`).
    /// `file_data_offset` is the byte offset of the CPIO file data within `initrd`.
    /// `file_len` is the exact file data length.
    ///
    /// The MemoryObject's physical address is the page-aligned start of the file data.
    /// Its length is `file_len` rounded up to the next page boundary.
    /// The returned cap has READ | MAP rights (no WRITE).
    pub(crate) fn create_initramfs_file_slice_mo(
        &mut self,
        initrd: &[u8],
        file_data_offset: usize,
        file_len: usize,
    ) -> Result<(u64, CapId), KernelError> {
        use crate::kernel::vm::PAGE_SIZE;
        if file_len == 0 {
            return Err(KernelError::Vm(VmError::Misaligned));
        }
        let file_end = file_data_offset.checked_add(file_len).ok_or(KernelError::WrongObject)?;
        if file_end > initrd.len() {
            return Err(KernelError::WrongObject);
        }
        // Compute physical address: translate initrd virtual pointer → physical.
        let initrd_virt_raw = initrd.as_ptr() as u64;
        let initrd_phys_base = Self::normalize_initrd_phys_ptr_static(initrd_virt_raw)
            .map_err(|_| KernelError::WrongObject)?;
        let file_phys_raw = initrd_phys_base.checked_add(file_data_offset as u64)
            .ok_or(KernelError::WrongObject)?;
        // Round physical address down to page boundary.
        let page_size = PAGE_SIZE as u64;
        let phys_page_start = file_phys_raw & !(page_size - 1);
        // Length: from page-aligned start through end of file data, rounded up.
        let offset_within_page = (file_phys_raw - phys_page_start) as usize;
        let len_pages = (offset_within_page + file_len + PAGE_SIZE - 1) / PAGE_SIZE * PAGE_SIZE;

        let kind = MemoryObjectKind::InitramfsFileSlice {
            initrd_offset: file_data_offset as u64,
            file_len: file_len as u64,
        };
        self.create_memory_object_with_len_and_kind(
            PhysAddr(phys_page_start),
            len_pages,
            kind,
        )
    }

    /// Translate an initrd virtual pointer to a physical address.
    /// Mirrors the kernel's local `normalize_initrd_phys_ptr` helper in syscall.rs.
    fn normalize_initrd_phys_ptr_static(raw_ptr: u64) -> Result<u64, KernelError> {
        let virt_base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE;
        let phys_base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_PHYS_BASE;
        if virt_base > phys_base && raw_ptr >= virt_base {
            let off = raw_ptr.checked_sub(virt_base).ok_or(KernelError::WrongObject)?;
            let phys = phys_base.checked_add(off).ok_or(KernelError::WrongObject)?;
            return Ok(phys);
        }
        if raw_ptr < virt_base || virt_base == phys_base {
            return Ok(raw_ptr);
        }
        Err(KernelError::WrongObject)
    }

    pub fn alloc_anonymous_memory_object(&mut self) -> Result<(u64, CapId), KernelError> {
        self.alloc_anonymous_memory_object_with_len(crate::kernel::vm::PAGE_SIZE)
    }

    pub(crate) fn alloc_user_data_frame(&mut self) -> Result<u64, KernelError> {
        let pa = self.with_memory_state_mut(|memory| {
            kernel_mut(&mut memory.frame_allocator)
                .alloc_frame()
                .map_err(|_| KernelError::MemoryObjectFull)
        })?;
        #[cfg(not(feature = "hosted-dev"))]
        if let Some((rs, re)) = crate::kernel::frame_allocator::is_pa_in_pt_pool(pa) {
            crate::yarm_log!(
                "PMEM_ALLOC_PT_POOL_BUG pa=0x{:x} pt_range=0x{:x}..0x{:x}",
                pa, rs, re
            );
            panic!("PMEM_ALLOC_PT_POOL_BUG: main frame allocator returned a PT-pool PA");
        }
        #[cfg(all(not(feature = "hosted-dev"), feature = "trace_frame_alloc"))]
        crate::yarm_log!("PMEM_ALLOC_FRAME pa=0x{:x} owner=user", pa);
        Ok(pa)
    }

    pub fn alloc_anonymous_memory_object_with_len(
        &mut self,
        len: usize,
    ) -> Result<(u64, CapId), KernelError> {
        if len == 0 {
            return Err(KernelError::Vm(VmError::Misaligned));
        }
        let pages = len.div_ceil(crate::kernel::vm::PAGE_SIZE);
        let total_len = pages * crate::kernel::vm::PAGE_SIZE;
        let phys = PhysAddr(self.with_memory_state_mut(|memory| {
            kernel_mut(&mut memory.frame_allocator)
                .alloc_contiguous(pages)
                .map_err(|err| match err {
                    FrameAllocError::OutOfMemory => KernelError::MemoryObjectFull,
                    _ => KernelError::Vm(VmError::Full),
                })
        })?);
        #[cfg(not(feature = "hosted-dev"))]
        if let Some((rs, re)) = crate::kernel::frame_allocator::is_pa_in_pt_pool(phys.0) {
            crate::yarm_log!(
                "PMEM_ALLOC_PT_POOL_BUG_CONTIG pa=0x{:x} pt_range=0x{:x}..0x{:x} pages={}",
                phys.0, rs, re, pages
            );
            panic!("PMEM_ALLOC_PT_POOL_BUG_CONTIG: main contiguous allocator returned a PT-pool PA");
        }
        #[cfg(all(not(feature = "hosted-dev"), feature = "trace_frame_alloc"))]
        crate::yarm_log!("PMEM_ALLOC_FRAME pa=0x{:x} owner=user_contig pages={}", phys.0, pages);
        self.create_memory_object_with_len(phys, total_len)
    }

    pub fn task_brk_bounds(&self, tid: u64) -> Option<(usize, usize)> {
        self.with_memory_state(|memory| {
            memory
                .brk_regions
                .iter()
                .flatten()
                .find(|entry| entry.tid.0 == tid)
                .map(|entry| (entry.base.0 as usize, entry.end.0 as usize))
        })
    }

    pub fn set_task_brk_bounds(
        &mut self,
        tid: u64,
        base: usize,
        end: usize,
    ) -> Result<(), KernelError> {
        self.with_tcbs(|tcbs| tcbs.iter().flatten().any(|tcb| tcb.tid.0 == tid))
            .then_some(())
            .ok_or(KernelError::TaskMissing)?;
        self.with_memory_state_mut(|memory| {
            if let Some(slot) = memory
                .brk_regions
                .iter_mut()
                .find(|slot| slot.is_some_and(|entry| entry.tid.0 == tid) || slot.is_none())
            {
                *slot = Some(super::BrkRegionRecord {
                    tid: crate::kernel::ipc::ThreadId(tid),
                    base: VirtAddr(base as u64),
                    end: VirtAddr(end as u64),
                });
                Ok(())
            } else {
                Err(KernelError::TaskTableFull)
            }
        })
    }

    pub(crate) fn resolve_memory_object_phys(
        &self,
        mem_cap: CapId,
        flags: PageFlags,
    ) -> Result<PhysAddr, KernelError> {
        let capability = self
            .capability_service()
            .resolve_current_task_capability(mem_cap)
            .ok_or(KernelError::InvalidCapability)?;
        let id = match capability.object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return Err(KernelError::WrongObject),
        };

        if flags.read && !capability.has_right(CapRights::READ) {
            return Err(KernelError::MissingRight);
        }
        if flags.write && !capability.has_right(CapRights::WRITE) {
            return Err(KernelError::MissingRight);
        }

        self.with_memory_state(|memory| {
            memory
                .memory_objects
                .iter()
                .flatten()
                .find(|entry| entry.id == id)
                .map(|entry| entry.phys)
                .ok_or(KernelError::MemoryObjectMissing)
        })
    }

    pub(crate) fn map_user_page_in_asid_raw(
        &mut self,
        asid: Asid,
        virt: VirtAddr,
        mapping: Mapping,
    ) -> Result<Option<Mapping>, KernelError> {
        if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
            crate::yarm_log!(
                "MAP_USER_RAW_BEGIN asid={} virt=0x{:x} phys=0x{:x} user={} rwx={}{}{}",
                asid.0,
                virt.0,
                mapping.phys.0,
                mapping.flags.user,
                if mapping.flags.read { "r" } else { "-" },
                if mapping.flags.write { "w" } else { "-" },
                if mapping.flags.execute { "x" } else { "-" }
            );
        }
        let old = self.with_user_spaces_mut(|spaces| {
            let aspace = spaces
                .get_mut(asid)
                .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
            if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
                crate::yarm_log!(
                    "MAP_USER_RAW_ASPACE asid={} aspace_asid={}",
                    asid.0,
                    aspace.asid().map(|asid| asid.0).unwrap_or(0)
                );
            }
            aspace.map_page(virt, mapping).map_err(KernelError::Vm)
        })?;
        let resolved = crate::arch::selected_isa::page_table::resolve_page(asid, virt).is_some();
        if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
            crate::yarm_log!(
                "MAP_USER_RAW_DONE asid={} virt=0x{:x} had_old={} resolve_ok={}",
                asid.0,
                virt.0,
                old.is_some(),
                resolved
            );
        }
        if let Some(old_mapping) = old {
            self.clear_cow_page(asid, virt);
            self.note_mapping_removed(old_mapping.phys);
            self.reclaim_memory_object_for_phys(old_mapping.phys);
        }
        if mapping.flags.write {
            self.clear_cow_page(asid, virt);
        }
        self.note_mapping_inserted(mapping.phys);
        Ok(old)
    }

    pub fn map_user_page_with_caps(
        &mut self,
        aspace_map_cap: CapId,
        mem_cap: CapId,
        virt: VirtAddr,
        flags: PageFlags,
    ) -> Result<Option<Mapping>, KernelError> {
        let phys = self.resolve_memory_object_phys(mem_cap, flags)?;
        self.map_user_page(aspace_map_cap, virt, Mapping { phys, flags })
    }

    /// Stage 5C explicit-ASID helper: map a page using a pre-resolved ASID.
    ///
    /// Equivalent to `map_user_page_in_current_asid_with_caps` but takes
    /// an explicit `asid` from the caller's `VmAnonMapPlan` instead of
    /// re-reading the scheduler (rank 1) and task (rank 2) state.
    ///
    /// Lock-domain flow: capability (rank 4) → vm (rank 5).
    /// No scheduler or task lock acquisition.
    pub(crate) fn map_user_page_in_asid_with_caps(
        &mut self,
        asid: Asid,
        mem_cap: CapId,
        virt: VirtAddr,
        flags: PageFlags,
    ) -> Result<Option<Mapping>, KernelError> {
        let phys = self.resolve_memory_object_phys(mem_cap, flags)?;
        self.map_user_page_in_asid_raw(asid, virt, Mapping { phys, flags })
    }

    /// Stage 5C explicit-ASID helper: check whether a page is mapped using a
    /// pre-resolved ASID.
    ///
    /// Equivalent to `is_user_page_mapped_in_current_asid` but takes an
    /// explicit `asid` instead of re-reading scheduler (rank 1) and task
    /// (rank 2) state.
    ///
    /// Lock-domain flow: vm (rank 5) read only.
    pub(crate) fn is_user_page_mapped_in_asid(
        &self,
        asid: Asid,
        virt: VirtAddr,
    ) -> Result<bool, KernelError> {
        if !virt.0.is_multiple_of(crate::kernel::vm::PAGE_SIZE as u64) {
            return Err(KernelError::WrongObject);
        }
        self.with_user_spaces(|spaces| {
            spaces
                .get(asid)
                .ok_or(KernelError::Vm(VmError::InvalidAsid))
                .map(|aspace| aspace.resolve(virt).is_some())
        })
    }

    pub fn unmap_user_page(
        &mut self,
        aspace_map_cap: CapId,
        virt: VirtAddr,
    ) -> Result<Option<Mapping>, KernelError> {
        let capability = self
            .capability_service()
            .resolve_current_task_capability(aspace_map_cap)
            .ok_or(KernelError::InvalidCapability)?;
        let asid = match capability.object {
            CapObject::AddressSpace { asid } => Asid(asid),
            _ => return Err(KernelError::WrongObject),
        };
        if !capability.has_right(CapRights::MAP) {
            return Err(KernelError::MissingRight);
        }
        let unmapped = self.with_user_spaces_mut(|spaces| {
            Ok::<_, KernelError>(
                spaces
                    .get_mut(asid)
                    .ok_or(KernelError::Vm(VmError::InvalidAsid))?
                    .unmap_page(virt),
            )
        })?;
        if let Some(mapping) = unmapped {
            self.clear_cow_page(asid, virt);
            self.note_mapping_removed(mapping.phys);
            self.reclaim_memory_object_for_phys(mapping.phys);
            self.request_live_asid_shootdown(asid, virt)?;
        }
        Ok(unmapped)
    }

    pub(crate) fn unmap_user_page_in_asid(
        &mut self,
        asid: Asid,
        virt: VirtAddr,
    ) -> Result<Option<Mapping>, KernelError> {
        let unmapped = self.with_user_spaces_mut(|spaces| {
            Ok::<_, KernelError>(
                spaces
                    .get_mut(asid)
                    .ok_or(KernelError::Vm(VmError::InvalidAsid))?
                    .unmap_page(virt),
            )
        })?;
        if let Some(mapping) = unmapped {
            self.clear_cow_page(asid, virt);
            self.note_mapping_removed(mapping.phys);
            self.reclaim_memory_object_for_phys(mapping.phys);
            self.request_live_asid_shootdown(asid, virt)?;
        }
        Ok(unmapped)
    }

    /// Stage 5E: Phase-1 unmap — removes page table entry and clears memory
    /// accounting (COW record + map_refcount), but defers TLB shootdown and
    /// frame reclamation to the caller.
    ///
    /// Returns `Ok(Some(plan))` if the page was mapped. The caller MUST:
    ///   1. IF `plan.target_cpu_bitmap != 0`: call `request_live_asid_shootdown`
    ///      on `(plan.asid, plan.virt)` before step 2.
    ///   2. Call `reclaim_memory_object_for_phys(plan.phys)` after step 1.
    ///
    /// Returns `Ok(None)` if the page was not present — idempotent, safe to call
    /// on lazy / never-faulted pages (same as `unmap_user_page_in_asid`).
    ///
    /// ## Lock sequence (all acquired+released sequentially, none simultaneously)
    ///   vm (rank 5)     — unmap page table entry
    ///   memory (rank 6) — clear_cow_page, note_mapping_removed
    ///   scheduler (rank 1) + task (rank 2) — compute_tlb_shootdown_request_plan
    ///
    /// Compared to `unmap_user_page_in_asid`, this omits:
    ///   - `reclaim_memory_object_for_phys`  (deferred to phase 3)
    ///   - `request_live_asid_shootdown`     (deferred to phase 2)
    ///
    /// ## Relationship to blocker #1
    ///
    /// The existing `unmap_user_page_in_asid` calls `reclaim_memory_object_for_phys`
    /// BEFORE `request_live_asid_shootdown`. Under the global lock this is safe.
    /// `unmap_page_phase1` + explicit shootdown + explicit reclaim is the pattern
    /// needed for global-lock-free correctness.
    pub(crate) fn unmap_page_phase1(
        &mut self,
        asid: Asid,
        virt: VirtAddr,
    ) -> Result<Option<super::TlbShootdownWaitPlan>, KernelError> {
        let unmapped = self.with_user_spaces_mut(|spaces| {
            Ok::<_, KernelError>(
                spaces
                    .get_mut(asid)
                    .ok_or(KernelError::Vm(VmError::InvalidAsid))?
                    .unmap_page(virt),
            )
        })?;
        let Some(mapping) = unmapped else {
            return Ok(None);
        };
        self.clear_cow_page(asid, virt);
        self.note_mapping_removed(mapping.phys);
        // Frame reclamation is intentionally NOT done here (deferred to phase 3).
        let req = self.compute_tlb_shootdown_request_plan(asid, virt);
        Ok(Some(super::TlbShootdownWaitPlan {
            asid: req.asid,
            virt: req.virt,
            target_cpu_bitmap: req.target_cpu_bitmap,
            requester: req.requester,
            phys: mapping.phys,
        }))
    }

    /// Stage 5F: Execute phases 2 and 3 of the two-phase unmap for a single page.
    ///
    /// This is the mandatory second step after `unmap_page_phase1`. It MUST be
    /// called for every `Some(plan)` returned by phase 1, and only after all
    /// vm/memory lock work for phase 1 is complete.
    ///
    /// ## Phase 2 — TLB shootdown (ipc rank 3)
    ///
    /// If `plan.target_cpu_bitmap == 0`, the fast path is taken: no cross-CPU
    /// notification is needed and the ipc lock is NOT acquired. In single-CPU
    /// environments (hosted-dev, BT2), this is always the fast path.
    ///
    /// If `plan.target_cpu_bitmap != 0`, `request_live_asid_shootdown` is called
    /// which acquires ipc(3) sequentially after vm(5)/memory(6) were released in
    /// phase 1. There is no simultaneous vm/memory/ipc nesting (see comment on
    /// `request_live_asid_shootdown` for the full rank-ordering characterisation).
    ///
    /// ## Phase 3 — frame reclamation (memory rank 6)
    ///
    /// `reclaim_memory_object_for_phys(plan.phys)` is called after the shootdown
    /// (or after confirming no shootdown is needed). This ensures the physical
    /// frame is not freed before all CPUs have invalidated their TLB entries for
    /// the removed mapping — the fix for Stage 5E blocker #1b.
    pub(crate) fn execute_tlb_shootdown_wait_plan(
        &mut self,
        plan: super::TlbShootdownWaitPlan,
    ) -> Result<(), KernelError> {
        // Phase 2: Fast path if no remote CPUs need notification.
        // The bitmap in the plan is the snapshot from phase 1; under the global
        // lock it is equivalent to recomputing now (no CPU can start running the
        // ASID between phase 1 and phase 2).
        if plan.target_cpu_bitmap != 0 {
            self.request_live_asid_shootdown(plan.asid, plan.virt)?;
        }
        // Phase 3: Reclaim the physical frame now that shootdown is complete (or
        // confirmed unnecessary). This ordering prevents UAF under global-lock removal.
        self.reclaim_memory_object_for_phys(plan.phys);
        Ok(())
    }

    /// Stage 11: Two-phase unmap of a contiguous VA range.
    ///
    /// For each page in `[base, base + len)`: calls `unmap_page_phase1` (PTE
    /// removal + `map_refcount--`), then `execute_tlb_shootdown_wait_plan`
    /// (TLB shootdown + deferred frame reclaim). Absent pages are silently
    /// skipped — `unmap_page_phase1` returns `Ok(None)` for them.
    ///
    /// Used by `purge_active_transfer_mappings_for_pid` and
    /// `revoke_active_transfer_mappings_for_cap` to replace the old one-phase
    /// `unmap_user_page_in_asid` pattern. Errors from either phase are swallowed
    /// (same policy as the one-phase path they replace).
    ///
    /// Lock-domain flow (each page, no simultaneous acquisition):
    ///   vm (5) → memory (6) → scheduler+task (1+2) [phase 1]
    ///   ipc (3) → memory (6) [phase 2]
    pub(crate) fn unmap_range_two_phase(&mut self, asid: Asid, base: usize, len: usize) {
        let end = base.saturating_add(len);
        let mut va = base;
        while va < end {
            if let Ok(Some(wait_plan)) = self.unmap_page_phase1(asid, VirtAddr(va as u64)) {
                let _ = self.execute_tlb_shootdown_wait_plan(wait_plan);
            }
            va = va.saturating_add(crate::kernel::vm::PAGE_SIZE);
        }
    }

    pub fn protect_user_page(
        &mut self,
        aspace_map_cap: CapId,
        virt: VirtAddr,
        new_flags: PageFlags,
    ) -> Result<Option<Mapping>, KernelError> {
        let capability = self
            .capability_service()
            .resolve_current_task_capability(aspace_map_cap)
            .ok_or(KernelError::InvalidCapability)?;
        let asid = match capability.object {
            CapObject::AddressSpace { asid } => Asid(asid),
            _ => return Err(KernelError::WrongObject),
        };
        if !capability.has_right(CapRights::MAP) {
            return Err(KernelError::MissingRight);
        }
        let (old, current_phys) = self.with_user_spaces_mut(|spaces| -> Result<_, KernelError> {
            let aspace = spaces
                .get_mut(asid)
                .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
            let current_phys = aspace
                .resolve(virt)
                .ok_or(KernelError::Vm(VmError::InvalidAsid))?
                .phys;
            let old = aspace
                .map_page(virt, Mapping { phys: current_phys, flags: new_flags })
                .map_err(KernelError::Vm)?;
            Ok((old, current_phys))
        })?;
        if let Some(old_mapping) = old {
            self.clear_cow_page(asid, virt);
            self.note_mapping_removed(old_mapping.phys);
            self.reclaim_memory_object_for_phys(old_mapping.phys);
        }
        if new_flags.write {
            self.clear_cow_page(asid, virt);
        }
        self.note_mapping_inserted(current_phys);
        Ok(old)
    }

    #[cfg(feature = "posix-compat")]
    pub(crate) fn protect_user_page_in_asid(
        &mut self,
        asid: Asid,
        virt: VirtAddr,
        new_flags: PageFlags,
    ) -> Result<Option<Mapping>, KernelError> {
        let (old, current_phys) = self.with_user_spaces_mut(|spaces| -> Result<_, KernelError> {
            let aspace = spaces
                .get_mut(asid)
                .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
            let current_phys = aspace
                .resolve(virt)
                .ok_or(KernelError::Vm(VmError::InvalidAsid))?
                .phys;
            let old = aspace
                .map_page(virt, Mapping { phys: current_phys, flags: new_flags })
                .map_err(KernelError::Vm)?;
            Ok((old, current_phys))
        })?;
        if let Some(old_mapping) = old {
            self.clear_cow_page(asid, virt);
            self.note_mapping_removed(old_mapping.phys);
            self.reclaim_memory_object_for_phys(old_mapping.phys);
        }
        if new_flags.write {
            self.clear_cow_page(asid, virt);
        }
        self.note_mapping_inserted(current_phys);
        Ok(old)
    }

    /// Return true if `asid` is in the live (non-retired) address space table.
    #[cfg(test)]
    pub(crate) fn asid_is_live_for_test(&self, asid: Asid) -> bool {
        self.with_user_spaces(|spaces| spaces.get(asid).is_some())
    }

    /// Return true if `asid` is in the retired ASID table awaiting shootdown ACKs.
    #[cfg(test)]
    pub(crate) fn asid_is_retired_for_test(&self, asid: Asid) -> bool {
        self.with_user_spaces(|spaces| spaces.retired_entry(asid).is_some())
    }

    /// Return the number of pages currently mapped in `asid`.
    #[cfg(test)]
    pub(crate) fn mapped_page_count_for_asid(&self, asid: Asid) -> usize {
        self.with_user_spaces(|spaces| {
            spaces.get(asid).map_or(0, |aspace| aspace.mappings())
        })
    }
}
