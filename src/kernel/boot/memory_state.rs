use super::{KernelError, KernelState, MemoryObject, kernel_mut};
use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
use crate::kernel::frame_allocator::FrameAllocError;
use crate::kernel::vm::{Asid, Mapping, PageFlags, PhysAddr, VirtAddr, VmError};

impl KernelState {
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

        let pending_cpu_bitmap = self.online_cpu_bitmap();
        self.user_spaces
            .destroy(asid, pending_cpu_bitmap)
            .map_err(KernelError::Vm)?;

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

    pub fn create_user_address_space(&mut self) -> Result<(Asid, CapId), KernelError> {
        let asid = self
            .user_spaces
            .create_user_space()
            .map_err(KernelError::Vm)?;
        let map_cap = self.mint_capability_for_current_context(Capability::new(
            CapObject::AddressSpace { asid: asid.0 },
            CapRights::MAP | CapRights::READ | CapRights::WRITE,
        ))?;
        Ok((asid, map_cap))
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

        let aspace = self
            .user_spaces
            .get_mut(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let old = aspace.map_page(virt, mapping).map_err(KernelError::Vm)?;
        if let Some(old_mapping) = old {
            self.note_mapping_removed(old_mapping.phys);
            self.reclaim_memory_object_for_phys(old_mapping.phys);
        }
        self.note_mapping_inserted(mapping.phys);
        Ok(old)
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
        if self.memory.memory_objects.iter().flatten().count()
            >= self.runtime_capacity_config().max_memory_objects
        {
            return Err(KernelError::MemoryObjectFull);
        }
        let id = self.memory.next_memory_object_id;
        self.memory.next_memory_object_id = self.memory.next_memory_object_id.wrapping_add(1);

        let slot = self
            .memory
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
        });

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
        let phys = PhysAddr(
            kernel_mut(&mut self.memory.frame_allocator)
                .alloc_contiguous(pages)
                .map_err(|err| match err {
                    FrameAllocError::OutOfMemory => KernelError::MemoryObjectFull,
                    _ => KernelError::Vm(VmError::Full),
                })?,
        );
        self.create_memory_object_with_len(phys, total_len)
    }

    pub fn task_brk_bounds(&self, tid: u64) -> Option<(usize, usize)> {
        self.memory
            .brk_regions
            .iter()
            .flatten()
            .find(|entry| entry.tid.0 == tid)
            .map(|entry| (entry.base.0 as usize, entry.end.0 as usize))
    }

    pub fn set_task_brk_bounds(
        &mut self,
        tid: u64,
        base: usize,
        end: usize,
    ) -> Result<(), KernelError> {
        let _ = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        if let Some(slot) = self
            .memory
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

        self.memory
            .memory_objects
            .iter()
            .flatten()
            .find(|entry| entry.id == id)
            .map(|entry| entry.phys)
            .ok_or(KernelError::MemoryObjectMissing)
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
        let aspace = self
            .user_spaces
            .get_mut(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let old = aspace
            .map_page(virt, Mapping { phys, flags })
            .map_err(KernelError::Vm)?;
        if let Some(old_mapping) = old {
            self.note_mapping_removed(old_mapping.phys);
            self.reclaim_memory_object_for_phys(old_mapping.phys);
        }
        self.note_mapping_inserted(phys);
        Ok(old)
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
            self.note_mapping_removed(mapping.phys);
            self.reclaim_memory_object_for_phys(mapping.phys);
        }
        Ok(unmapped)
    }

    #[cfg(feature = "linux-compat")]
    pub(crate) fn map_user_page_in_asid_with_caps(
        &mut self,
        asid: Asid,
        mem_cap: CapId,
        virt: VirtAddr,
        flags: PageFlags,
    ) -> Result<Option<Mapping>, KernelError> {
        let phys = self.resolve_memory_object_phys(mem_cap, flags)?;
        let aspace = self
            .user_spaces
            .get_mut(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let old = aspace
            .map_page(virt, Mapping { phys, flags })
            .map_err(KernelError::Vm)?;
        if let Some(old_mapping) = old {
            self.note_mapping_removed(old_mapping.phys);
            self.reclaim_memory_object_for_phys(old_mapping.phys);
        }
        self.note_mapping_inserted(phys);
        Ok(old)
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
            self.note_mapping_removed(mapping.phys);
            self.reclaim_memory_object_for_phys(mapping.phys);
        }
        Ok(unmapped)
    }

    #[cfg(feature = "linux-compat")]
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
            self.note_mapping_removed(mapping.phys);
            self.reclaim_memory_object_for_phys(mapping.phys);
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
            self.note_mapping_removed(old_mapping.phys);
            self.reclaim_memory_object_for_phys(old_mapping.phys);
        }
        self.note_mapping_inserted(current.phys);
        Ok(old)
    }

    #[cfg(feature = "linux-compat")]
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
            self.note_mapping_removed(old_mapping.phys);
            self.reclaim_memory_object_for_phys(old_mapping.phys);
        }
        self.note_mapping_inserted(current.phys);
        Ok(old)
    }
}
