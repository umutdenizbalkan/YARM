use super::{
    DeviceServerDelegation, DriverBundlePlan, DriverDelegationBundle, DriverRecord,
    IpcPathTelemetry, KernelError, KernelState,
};
use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
use crate::kernel::ipc::ThreadId;
use crate::kernel::vm::VmError;

impl KernelState {
    pub fn register_driver(&mut self, tid: u64) -> Result<(), KernelError> {
        let _ = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        if self
            .drivers
            .driver_records
            .iter()
            .flatten()
            .any(|record| record.tid == ThreadId(tid))
        {
            return Ok(());
        }

        if let Some(slot) = self
            .drivers
            .driver_records
            .iter_mut()
            .find(|slot| slot.is_none())
        {
            *slot = Some(DriverRecord {
                tid: ThreadId(tid),
                irq_cap: None,
                dma_cap: None,
                dma_iova_base: None,
                dma_iova_len: None,
                iova_space_cap: None,
            });
            Ok(())
        } else {
            Err(KernelError::TaskTableFull)
        }
    }

    pub fn grant_driver_irq(&mut self, tid: u64, irq_cap: CapId) -> Result<(), KernelError> {
        let capability = self
            .cspace
            .get(irq_cap)
            .ok_or(KernelError::InvalidCapability)?;
        match capability.object {
            CapObject::Irq { .. } => {}
            _ => return Err(KernelError::WrongObject),
        }
        if !capability.has_right(CapRights::SIGNAL) {
            return Err(KernelError::MissingRight);
        }
        let record = self
            .drivers
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;
        record.irq_cap = Some(irq_cap);
        Ok(())
    }

    pub fn mint_irq_cap(&mut self, line: u16) -> Result<CapId, KernelError> {
        self.cspace
            .mint(Capability::new(CapObject::Irq { line }, CapRights::SIGNAL))
            .map_err(|_| KernelError::CapabilityFull)
    }

    pub fn create_iova_space_cap(&mut self) -> Result<CapId, KernelError> {
        let id = self.drivers.next_iova_space_id;
        self.drivers.next_iova_space_id =
            self.drivers.next_iova_space_id.checked_add(1).unwrap_or(1);
        self.cspace
            .mint(Capability::new(CapObject::IovaSpace { id }, CapRights::MAP))
            .map_err(|_| KernelError::CapabilityFull)
    }

    pub fn grant_driver_iova_space(
        &mut self,
        tid: u64,
        iova_cap: CapId,
    ) -> Result<(), KernelError> {
        let capability = self
            .cspace
            .get(iova_cap)
            .ok_or(KernelError::InvalidCapability)?;
        match capability.object {
            CapObject::IovaSpace { .. } => {}
            _ => return Err(KernelError::WrongObject),
        }

        let record = self
            .drivers
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;
        record.iova_space_cap = Some(iova_cap);
        Ok(())
    }

    pub fn mint_dma_region_cap(
        &mut self,
        mem_cap: CapId,
        offset: usize,
        len: usize,
    ) -> Result<CapId, KernelError> {
        let capability = self
            .cspace
            .get(mem_cap)
            .ok_or(KernelError::InvalidCapability)?;
        let id = match capability.object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return Err(KernelError::WrongObject),
        };
        if !capability.has_right(CapRights::MAP)
            || !capability.has_right(CapRights::READ)
            || !capability.has_right(CapRights::WRITE)
        {
            return Err(KernelError::MissingRight);
        }

        if !offset.is_multiple_of(crate::kernel::vm::PAGE_SIZE)
            || !len.is_multiple_of(crate::kernel::vm::PAGE_SIZE)
            || len == 0
        {
            return Err(KernelError::Vm(VmError::Misaligned));
        }
        let parent_len = self
            .memory
            .memory_objects
            .iter()
            .flatten()
            .find(|entry| entry.id == id)
            .map(|entry| entry.len)
            .ok_or(KernelError::MemoryObjectMissing)?;

        if offset
            .checked_add(len)
            .ok_or(KernelError::Vm(VmError::Misaligned))?
            > parent_len
        {
            return Err(KernelError::WrongObject);
        }

        self.cspace
            .mint(Capability::new(
                CapObject::DmaRegion {
                    id,
                    offset: offset as u64,
                    len: len as u64,
                },
                CapRights::MAP | CapRights::READ | CapRights::WRITE,
            ))
            .map_err(|_| KernelError::CapabilityFull)
    }

    pub fn grant_driver_dma(&mut self, tid: u64, dma_cap: CapId) -> Result<(), KernelError> {
        let capability = self
            .cspace
            .get(dma_cap)
            .ok_or(KernelError::InvalidCapability)?;
        match capability.object {
            CapObject::DmaRegion { len, .. } if len > 0 => {}
            CapObject::DmaRegion { .. } => return Err(KernelError::WrongObject),
            _ => return Err(KernelError::WrongObject),
        }

        let record = self
            .drivers
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;
        record.dma_cap = Some(dma_cap);
        Ok(())
    }

    pub fn delegate_device_server_caps(
        &mut self,
        plan: DeviceServerDelegation,
    ) -> Result<(CapId, CapId), KernelError> {
        self.register_driver(plan.server_tid.0)?;

        let irq_cap = self.mint_irq_cap(plan.irq_line)?;
        self.grant_driver_irq(plan.server_tid.0, irq_cap)?;

        let dma_cap = self.mint_dma_region_cap(plan.mem_cap, plan.dma_offset, plan.dma_len)?;
        self.grant_driver_dma(plan.server_tid.0, dma_cap)?;

        self.grant_driver_iova_space(plan.server_tid.0, plan.iova_cap)?;
        self.configure_driver_dma_window(plan.server_tid.0, plan.iova_base, plan.iova_len)?;

        Ok((irq_cap, dma_cap))
    }

    pub fn delegate_driver_bundle(
        &mut self,
        plan: DriverBundlePlan,
    ) -> Result<DriverDelegationBundle, KernelError> {
        let (irq_cap, dma_cap) = self.delegate_device_server_caps(DeviceServerDelegation {
            server_tid: plan.server_tid,
            irq_line: plan.irq_line,
            mem_cap: plan.mem_cap,
            dma_offset: 0,
            dma_len: super::super::vm::PAGE_SIZE,
            iova_cap: plan.iova_cap,
            iova_base: plan.iova_base,
            iova_len: plan.iova_len,
        })?;
        Ok(DriverDelegationBundle {
            irq_cap,
            dma_cap,
            iova_cap: plan.iova_cap,
        })
    }

    pub fn ipc_path_telemetry(&self) -> IpcPathTelemetry {
        self.ipc.telemetry
    }

    pub fn validate_driver_bundle_live(
        &self,
        tid: u64,
        bundle: DriverDelegationBundle,
    ) -> Result<(), KernelError> {
        let record = self
            .drivers
            .driver_records
            .iter()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;

        if record.irq_cap != Some(bundle.irq_cap)
            || record.dma_cap != Some(bundle.dma_cap)
            || record.iova_space_cap != Some(bundle.iova_cap)
        {
            return Err(KernelError::StaleCapability);
        }

        // Driver delegation records are kernel-owned; validate against global caps.
        if self.kernel_global_capability(bundle.irq_cap).is_none()
            || self.kernel_global_capability(bundle.dma_cap).is_none()
            || self.kernel_global_capability(bundle.iova_cap).is_none()
        {
            return Err(KernelError::StaleCapability);
        }

        Ok(())
    }

    pub fn configure_driver_dma_window(
        &mut self,
        tid: u64,
        iova_base: usize,
        iova_len: usize,
    ) -> Result<(), KernelError> {
        if !iova_base.is_multiple_of(super::super::vm::PAGE_SIZE)
            || !iova_len.is_multiple_of(super::super::vm::PAGE_SIZE)
            || iova_len == 0
        {
            return Err(KernelError::Vm(VmError::Misaligned));
        }

        let record = self
            .drivers
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;
        record.dma_iova_base = Some(iova_base);
        record.dma_iova_len = Some(iova_len);
        Ok(())
    }

    pub fn validate_driver_dma_iova(
        &self,
        tid: u64,
        iova_base: usize,
        iova_len: usize,
    ) -> Result<(), KernelError> {
        if !iova_base.is_multiple_of(super::super::vm::PAGE_SIZE)
            || !iova_len.is_multiple_of(super::super::vm::PAGE_SIZE)
            || iova_len == 0
        {
            return Err(KernelError::Vm(VmError::Misaligned));
        }
        let record = self
            .drivers
            .driver_records
            .iter()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;

        if record.iova_space_cap.is_none() {
            return Err(KernelError::WrongObject);
        }

        match (record.dma_iova_base, record.dma_iova_len) {
            (Some(base), Some(len)) => {
                let end = iova_base
                    .checked_add(iova_len)
                    .ok_or(KernelError::WrongObject)?;
                let window_end = base.checked_add(len).ok_or(KernelError::WrongObject)?;
                if iova_base < base || end > window_end {
                    return Err(KernelError::WrongObject);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub fn detach_driver_iova_space(&mut self, tid: u64) -> Result<(), KernelError> {
        let record = self
            .drivers
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;
        record.iova_space_cap = None;
        record.dma_iova_base = None;
        record.dma_iova_len = None;
        Ok(())
    }

    pub fn revoke_driver_runtime_caps(&mut self, tid: u64) -> Result<(), KernelError> {
        let record = self
            .drivers
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;

        let irq_cap = record.irq_cap.take();
        let dma_cap = record.dma_cap.take();
        let iova_cap = record.iova_space_cap.take();
        record.dma_iova_base = None;
        record.dma_iova_len = None;

        if let Some(cap) = irq_cap {
            // Runtime bundle capabilities are globally minted and revoked by kernel policy.
            let _ = self.revoke_kernel_global_capability(cap);
        }
        if let Some(cap) = dma_cap {
            let _ = self.revoke_kernel_global_capability(cap);
        }
        if let Some(cap) = iova_cap {
            let _ = self.revoke_kernel_global_capability(cap);
        }
        Ok(())
    }
}
