// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState, MemoryObject, kernel_mut, kernel_ref};
use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
use crate::kernel::frame_allocator::FrameAllocError;
use crate::kernel::scheduler::CpuId;
use crate::kernel::topology::CpuBitmap;
use crate::kernel::vm::{Asid, Mapping, MappingEntry, PageFlags, PhysAddr, VirtAddr, VmError};

impl KernelState {
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
        let targets = self.live_cpu_bitmap_for_asid(asid);
        if targets == 0 {
            return Ok(());
        }
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
                },
            )?;
        }
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
            self.map_user_page_in_asid_raw(
                child_asid,
                virt,
                Mapping {
                    phys: mapping.phys,
                    flags: shared_flags,
                },
            )?;
            if mapping.flags.write {
                self.map_user_page_in_asid_raw(
                    parent_asid,
                    virt,
                    Mapping {
                        phys: mapping.phys,
                        flags: shared_flags,
                    },
                )?;
                self.mark_cow_page(parent_asid, virt)?;
                self.mark_cow_page(child_asid, virt)?;
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
            });
            Ok::<u64, KernelError>(id)
        })?;

        let cap = self.mint_capability_for_current_context(Capability::new(
            CapObject::MemoryObject { id },
            CapRights::READ | CapRights::WRITE | CapRights::MAP,
        ))?;

        Ok((id, cap))
    }

    pub fn alloc_anonymous_memory_object(&mut self) -> Result<(u64, CapId), KernelError> {
        self.alloc_anonymous_memory_object_with_len(crate::kernel::vm::PAGE_SIZE)
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

    fn resolve_memory_object_phys(
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

    fn map_user_page_in_asid_raw(
        &mut self,
        asid: Asid,
        virt: VirtAddr,
        mapping: Mapping,
    ) -> Result<Option<Mapping>, KernelError> {
        let old = self.with_user_spaces_mut(|spaces| {
            let aspace = spaces
                .get_mut(asid)
                .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
            aspace.map_page(virt, mapping).map_err(KernelError::Vm)
        })?;
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
