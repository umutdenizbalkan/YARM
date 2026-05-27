// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState, MemoryObject, MemoryObjectKind, kernel_mut, kernel_ref};
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
            if memory
                .cow_pages
                .iter()
                .flatten()
                .any(|entry| entry.asid == asid && entry.virt == virt)
            {
                return Ok(());
            }
            let Some(slot) = kernel_mut(&mut memory.cow_pages)
                .iter_mut()
                .find(|slot| slot.is_none())
            else {
                return Err(KernelError::MemoryObjectFull);
            };
            *slot = Some(super::CowPageRecord { asid, virt });
            Ok(())
        })
    }

    fn clear_cow_page(&mut self, asid: Asid, virt: VirtAddr) {
        self.with_memory_state_mut(|memory| {
            for slot in kernel_mut(&mut memory.cow_pages).iter_mut() {
                if matches!(
                    slot.as_ref(),
                    Some(entry) if entry.asid == asid && entry.virt == virt
                ) {
                    *slot = None;
                }
            }
        });
    }

    fn clear_cow_pages_for_asid(&mut self, asid: Asid) {
        self.with_memory_state_mut(|memory| {
            for slot in kernel_mut(&mut memory.cow_pages).iter_mut() {
                if matches!(slot.as_ref(), Some(entry) if entry.asid == asid) {
                    *slot = None;
                }
            }
        });
    }

    pub(crate) fn is_cow_page(&self, asid: Asid, virt: VirtAddr) -> bool {
        self.with_memory_state(|memory| {
            kernel_ref(&memory.cow_pages)
                .iter()
                .flatten()
                .any(|entry| entry.asid == asid && entry.virt == virt)
        })
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
        for mapping in drained.into_iter().flatten() {
            self.note_mapping_removed(mapping.phys);
            self.reclaim_memory_object_for_phys(mapping.phys);
        }

        for cpu in 0..u64::BITS as usize {
            let cpu_bit = 1u64 << cpu;
            if (pending_cpu_bitmap & cpu_bit) == 0 {
                continue;
            }
            self.submit_cross_cpu_work(
                crate::kernel::scheduler::CpuId(cpu as u8),
                crate::kernel::smp::WorkItem::TlbShootdown {
                    asid,
                    va_range: None,
                    requester: None,
                    sequence: 0,
                },
            )?;
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

        let cleanup_failed_child_clone = |state: &mut Self| {
            let _ = state.destroy_user_address_space_by_asid(child_asid);
        };

        let mut index = 0usize;
        while let Some(MappingEntry { virt, mapping }) = self.with_user_spaces(|spaces| {
            spaces
                .get(parent_asid)
                .and_then(|aspace| aspace.mapping_at(index))
        }) {
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
            )
            {
                cleanup_failed_child_clone(self);
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
                    cleanup_failed_child_clone(self);
                    return Err(err);
                }
                if let Err(err) = self.mark_cow_page(parent_asid, virt) {
                    cleanup_failed_child_clone(self);
                    return Err(err);
                }
                if let Err(err) = self.mark_cow_page(child_asid, virt) {
                    cleanup_failed_child_clone(self);
                    return Err(err);
                }
            }
            #[cfg(feature = "hosted-dev")]
            self.with_memory_state_mut(|memory| {
                for offset in 0..crate::kernel::vm::PAGE_SIZE {
                    let from = (parent_asid.0, virt.0 + offset as u64);
                    let to = (child_asid.0, virt.0 + offset as u64);
                    if let Some(value) = memory.user_memory.get(&from).copied() {
                        memory.user_memory.insert(to, value);
                    }
                }
            });
            index += 1;
        }

        Ok(child_asid)
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
        let (_id, mem_cap) = self.alloc_anonymous_memory_object()?;
        let new_phys = self.resolve_memory_object_phys(mem_cap, PageFlags::USER_RW)?;
        let mut flags = mapping.flags;
        flags.write = true;
        self.map_user_page_in_asid_raw(
            asid,
            page,
            Mapping {
                phys: new_phys,
                flags,
            },
        )
        .map(|_| ())?;
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

    pub(crate) fn map_user_page_in_current_asid_with_caps(
        &mut self,
        mem_cap: CapId,
        virt: VirtAddr,
        flags: PageFlags,
    ) -> Result<Option<Mapping>, KernelError> {
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        let phys = self.resolve_memory_object_phys(mem_cap, flags)?;
        self.map_user_page_in_asid_raw(asid, virt, Mapping { phys, flags })
    }

    pub(crate) fn unmap_user_page_in_current_asid(
        &mut self,
        virt: VirtAddr,
    ) -> Result<Option<Mapping>, KernelError> {
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        let aspace = self
            .user_spaces
            .get_mut(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let unmapped = aspace.unmap_page(virt);
        if let Some(mapping) = unmapped {
            self.clear_cow_page(asid, virt);
            self.note_mapping_removed(mapping.phys);
            self.reclaim_memory_object_for_phys(mapping.phys);
            self.request_live_asid_shootdown(asid, virt)?;
        }
        Ok(unmapped)
    }

    pub(crate) fn is_user_page_mapped_in_current_asid(
        &self,
        virt: VirtAddr,
    ) -> Result<bool, KernelError> {
        if !virt.0.is_multiple_of(crate::kernel::vm::PAGE_SIZE as u64) {
            return Err(KernelError::WrongObject);
        }
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        let aspace = self
            .user_spaces
            .get(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        Ok(aspace.resolve(virt).is_some())
    }

    #[cfg(feature = "posix-compat")]
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
        let aspace = self
            .user_spaces
            .get_mut(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let unmapped = aspace.unmap_page(virt);
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
        let aspace = self
            .user_spaces
            .get_mut(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let unmapped = aspace.unmap_page(virt);
        if let Some(mapping) = unmapped {
            self.clear_cow_page(asid, virt);
            self.note_mapping_removed(mapping.phys);
            self.reclaim_memory_object_for_phys(mapping.phys);
            self.request_live_asid_shootdown(asid, virt)?;
        }
        Ok(unmapped)
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
        let aspace = self
            .user_spaces
            .get_mut(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let current = aspace
            .resolve(virt)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let old = aspace
            .map_page(
                virt,
                Mapping {
                    phys: current.phys,
                    flags: new_flags,
                },
            )
            .map_err(KernelError::Vm)?;
        if let Some(old_mapping) = old {
            self.clear_cow_page(asid, virt);
            self.note_mapping_removed(old_mapping.phys);
            self.reclaim_memory_object_for_phys(old_mapping.phys);
        }
        if new_flags.write {
            self.clear_cow_page(asid, virt);
        }
        self.note_mapping_inserted(current.phys);
        Ok(old)
    }

    #[cfg(feature = "posix-compat")]
    pub(crate) fn protect_user_page_in_asid(
        &mut self,
        asid: Asid,
        virt: VirtAddr,
        new_flags: PageFlags,
    ) -> Result<Option<Mapping>, KernelError> {
        let aspace = self
            .user_spaces
            .get_mut(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let current = aspace
            .resolve(virt)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let old = aspace
            .map_page(
                virt,
                Mapping {
                    phys: current.phys,
                    flags: new_flags,
                },
            )
            .map_err(KernelError::Vm)?;
        if let Some(old_mapping) = old {
            self.clear_cow_page(asid, virt);
            self.note_mapping_removed(old_mapping.phys);
            self.reclaim_memory_object_for_phys(old_mapping.phys);
        }
        if new_flags.write {
            self.clear_cow_page(asid, virt);
        }
        self.note_mapping_inserted(current.phys);
        Ok(old)
    }
}
