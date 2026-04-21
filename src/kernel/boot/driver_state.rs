// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{
    DeviceServerDelegation, DriverBundlePlan, DriverDelegationBundle, DriverRecord,
    IpcPathTelemetry, KernelError, KernelState,
};
use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
use crate::kernel::ipc::ThreadId;
use crate::kernel::vm::VmError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverRedelegationDebug {
    pub current_tid: Option<u64>,
    pub source_tid: u64,
    pub target_tid: u64,
    pub source_task_exists: bool,
    pub target_task_exists: bool,
    pub source_cnode: Option<crate::kernel::capabilities::CNodeId>,
    pub target_cnode: Option<crate::kernel::capabilities::CNodeId>,
    pub source_has_mem_cap: bool,
    pub source_has_iova_cap: bool,
    pub target_driver_registered: bool,
}

impl KernelState {
    fn owner_tid_for_cap(&self, cap: CapId) -> Option<u64> {
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .map(|tcb| tcb.tid.0)
                .find(|tid| self.task_capability(*tid, cap).is_some())
        })
    }

    pub fn debug_driver_redelegation_context(
        &self,
        source_tid: u64,
        target_tid: u64,
        mem_cap: CapId,
        iova_cap: CapId,
    ) -> DriverRedelegationDebug {
        let source_cnode = self.task_cnode(source_tid);
        let target_cnode = self.task_cnode(target_tid);
        DriverRedelegationDebug {
            current_tid: self.current_tid(),
            source_tid,
            target_tid,
            source_task_exists: self.task_status(source_tid).is_some(),
            target_task_exists: self.task_status(target_tid).is_some(),
            source_cnode,
            target_cnode,
            source_has_mem_cap: self.task_capability(source_tid, mem_cap).is_some(),
            source_has_iova_cap: self.task_capability(source_tid, iova_cap).is_some(),
            target_driver_registered: self.with_driver_state(|driver| {
                driver
                    .driver_records
                    .iter()
                    .flatten()
                    .any(|record| record.tid == ThreadId(target_tid))
            }),
        }
    }

    pub fn register_driver(&mut self, tid: u64) -> Result<(), KernelError> {
        self.with_tcbs(|tcbs| tcbs.iter().flatten().any(|tcb| tcb.tid.0 == tid))
            .then_some(())
            .ok_or(KernelError::TaskMissing)?;
        if self.with_driver_state(|driver| {
            driver
                .driver_records
                .iter()
                .flatten()
                .any(|record| record.tid == ThreadId(tid))
        }) {
            return Ok(());
        }
        if self.with_driver_state(|driver| driver.driver_records.iter().flatten().count())
            >= self.runtime_capacity_config().max_drivers
        {
            return Err(KernelError::TaskTableFull);
        }

        self.with_driver_state_mut(|driver| {
            if let Some(slot) = driver.driver_records.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(DriverRecord {
                    tid: ThreadId(tid),
                    irq_caps: [None; super::MAX_DRIVER_IRQ_CAPS],
                    dma_caps: [None; super::MAX_DRIVER_DMA_CAPS],
                    dma_iova_base: None,
                    dma_iova_len: None,
                    iova_space_cap: None,
                });
                Ok(())
            } else {
                Err(KernelError::TaskTableFull)
            }
        })
    }

    pub fn grant_driver_irq(&mut self, tid: u64, irq_cap: CapId) -> Result<CapId, KernelError> {
        let source_tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        self.grant_driver_irq_from(source_tid, tid, irq_cap)
    }

    fn grant_driver_irq_from(
        &mut self,
        source_tid: u64,
        tid: u64,
        irq_cap: CapId,
    ) -> Result<CapId, KernelError> {
        let capability = self.resolve_capability_for_task(source_tid, irq_cap)?;
        match capability.object {
            CapObject::Irq { .. } => {}
            _ => return Err(KernelError::WrongObject),
        }
        if !capability.has_right(CapRights::SIGNAL) {
            return Err(KernelError::MissingRight);
        }
        let delegated_cap = self
            .capability_service_mut()
            .grant_task_to_task_with_rights(source_tid, irq_cap, tid, CapRights::SIGNAL)?;
        self.with_driver_state_mut(|driver| {
            let record = driver
                .driver_records
                .iter_mut()
                .flatten()
                .find(|record| record.tid == ThreadId(tid))
                .ok_or(KernelError::TaskMissing)?;
            if record.irq_caps.contains(&Some(delegated_cap)) {
                return Ok(delegated_cap);
            }
            if let Some(slot) = record.irq_caps.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(delegated_cap);
                return Ok(delegated_cap);
            }
            Err(KernelError::TaskTableFull)
        })
    }

    pub fn mint_irq_cap(&mut self, line: u16) -> Result<CapId, KernelError> {
        self.mint_capability_for_current_context(Capability::new(
            CapObject::Irq { line },
            CapRights::SIGNAL,
        ))
    }

    fn mint_irq_cap_for_task(&mut self, source_tid: u64, line: u16) -> Result<CapId, KernelError> {
        let source_cnode = self.task_cnode(source_tid).ok_or(KernelError::TaskMissing)?;
        self.mint_capability_in_cnode(
            source_cnode,
            Capability::new(CapObject::Irq { line }, CapRights::SIGNAL),
        )
    }

    pub fn create_iova_space_cap(&mut self) -> Result<CapId, KernelError> {
        let id = self.with_driver_state_mut(|driver| {
            let id = driver.next_iova_space_id;
            driver.next_iova_space_id = driver.next_iova_space_id.checked_add(1).unwrap_or(1);
            id
        });
        self.mint_capability_for_current_context(Capability::new(
            CapObject::IovaSpace { id },
            CapRights::MAP,
        ))
    }

    pub fn grant_driver_iova_space(
        &mut self,
        tid: u64,
        iova_cap: CapId,
    ) -> Result<CapId, KernelError> {
        let source_tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        self.grant_driver_iova_space_from(source_tid, tid, iova_cap)
    }

    fn grant_driver_iova_space_from(
        &mut self,
        source_tid: u64,
        tid: u64,
        iova_cap: CapId,
    ) -> Result<CapId, KernelError> {
        let capability = self.resolve_capability_for_task(source_tid, iova_cap)?;
        match capability.object {
            CapObject::IovaSpace { .. } => {}
            _ => return Err(KernelError::WrongObject),
        }
        let delegated_cap = self
            .capability_service_mut()
            .grant_task_to_task_with_rights(source_tid, iova_cap, tid, CapRights::MAP)?;
        self.with_driver_state_mut(|driver| {
            let record = driver
                .driver_records
                .iter_mut()
                .flatten()
                .find(|record| record.tid == ThreadId(tid))
                .ok_or(KernelError::TaskMissing)?;
            record.iova_space_cap = Some(delegated_cap);
            Ok(delegated_cap)
        })
    }

    pub fn mint_dma_region_cap(
        &mut self,
        mem_cap: CapId,
        offset: usize,
        len: usize,
    ) -> Result<CapId, KernelError> {
        let source_tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        self.mint_dma_region_cap_for_task(source_tid, mem_cap, offset, len)
    }

    fn mint_dma_region_cap_for_task(
        &mut self,
        source_tid: u64,
        mem_cap: CapId,
        offset: usize,
        len: usize,
    ) -> Result<CapId, KernelError> {
        let capability = self.resolve_capability_for_task(source_tid, mem_cap)?;
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

        let source_cnode = self.task_cnode(source_tid).ok_or(KernelError::TaskMissing)?;
        self.mint_capability_in_cnode(
            source_cnode,
            Capability::new(
            CapObject::DmaRegion {
                id,
                offset: offset as u64,
                len: len as u64,
            },
            CapRights::MAP | CapRights::READ | CapRights::WRITE,
            ),
        )
    }

    pub fn grant_driver_dma(&mut self, tid: u64, dma_cap: CapId) -> Result<CapId, KernelError> {
        let source_tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        self.grant_driver_dma_from(source_tid, tid, dma_cap)
    }

    fn grant_driver_dma_from(
        &mut self,
        source_tid: u64,
        tid: u64,
        dma_cap: CapId,
    ) -> Result<CapId, KernelError> {
        let capability = self.resolve_capability_for_task(source_tid, dma_cap)?;
        match capability.object {
            CapObject::DmaRegion { len, .. } if len > 0 => {}
            CapObject::DmaRegion { .. } => return Err(KernelError::WrongObject),
            _ => return Err(KernelError::WrongObject),
        }
        let delegated_cap = self
            .capability_service_mut()
            .grant_task_to_task_with_rights(
                source_tid,
                dma_cap,
                tid,
                CapRights::MAP | CapRights::READ | CapRights::WRITE,
            )?;
        self.with_driver_state_mut(|driver| {
            let record = driver
                .driver_records
                .iter_mut()
                .flatten()
                .find(|record| record.tid == ThreadId(tid))
                .ok_or(KernelError::TaskMissing)?;
            if record.dma_caps.contains(&Some(delegated_cap)) {
                return Ok(delegated_cap);
            }
            if let Some(slot) = record.dma_caps.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(delegated_cap);
                return Ok(delegated_cap);
            }
            Err(KernelError::TaskTableFull)
        })
    }

    pub fn delegate_device_server_caps(
        &mut self,
        plan: DeviceServerDelegation,
    ) -> Result<(CapId, CapId, CapId), KernelError> {
        self.register_driver(plan.server_tid.0)?;
        let source_tid = self
            .current_tid()
            .or_else(|| self.owner_tid_for_cap(plan.mem_cap))
            .or_else(|| self.owner_tid_for_cap(plan.iova_cap))
            .ok_or(KernelError::TaskMissing)?;

        let source_irq_cap = self.mint_irq_cap_for_task(source_tid, plan.irq_line)?;
        let irq_cap = self.grant_driver_irq_from(source_tid, plan.server_tid.0, source_irq_cap)?;

        let source_dma_cap =
            self.mint_dma_region_cap_for_task(source_tid, plan.mem_cap, plan.dma_offset, plan.dma_len)?;
        let dma_cap = self.grant_driver_dma_from(source_tid, plan.server_tid.0, source_dma_cap)?;

        let iova_cap = self.grant_driver_iova_space_from(source_tid, plan.server_tid.0, plan.iova_cap)?;
        self.configure_driver_dma_window(plan.server_tid.0, plan.iova_base, plan.iova_len)?;

        Ok((irq_cap, dma_cap, iova_cap))
    }

    pub fn delegate_driver_bundle(
        &mut self,
        plan: DriverBundlePlan,
    ) -> Result<DriverDelegationBundle, KernelError> {
        let (irq_cap, dma_cap, iova_cap) =
            self.delegate_device_server_caps(DeviceServerDelegation {
                server_tid: plan.server_tid,
                irq_line: plan.irq_line,
                mem_cap: plan.mem_cap,
                dma_offset: 0,
                dma_len: plan.dma_len,
                iova_cap: plan.iova_cap,
                iova_base: plan.iova_base,
                iova_len: plan.iova_len,
            })?;
        Ok(DriverDelegationBundle {
            irq_cap,
            dma_cap,
            iova_cap,
        })
    }

    pub fn delegate_driver_bundle_checked(
        &mut self,
        plan: DriverBundlePlan,
    ) -> Result<DriverDelegationBundle, KernelError> {
        let bundle = self.delegate_driver_bundle(plan)?;
        self.validate_driver_bundle_live(plan.server_tid.0, bundle)?;
        self.validate_driver_dma_iova(plan.server_tid.0, plan.iova_base, plan.dma_len)?;
        Ok(bundle)
    }

    pub fn redelegate_driver_bundle(
        &mut self,
        plan: DriverBundlePlan,
    ) -> Result<DriverDelegationBundle, KernelError> {
        self.revoke_driver_runtime_caps(plan.server_tid.0)?;
        self.delegate_driver_bundle_checked(plan)
    }

    pub fn ipc_path_telemetry(&self) -> IpcPathTelemetry {
        self.with_ipc_state(|ipc| ipc.telemetry)
    }

    pub fn validate_driver_bundle_live(
        &self,
        tid: u64,
        bundle: DriverDelegationBundle,
    ) -> Result<(), KernelError> {
        let record = self.with_driver_state(|driver| {
            driver
                .driver_records
                .iter()
                .flatten()
                .find(|record| record.tid == ThreadId(tid))
                .copied()
        });
        let record = record.ok_or(KernelError::TaskMissing)?;

        if !record.irq_caps.contains(&Some(bundle.irq_cap))
            || !record.dma_caps.contains(&Some(bundle.dma_cap))
            || record.iova_space_cap != Some(bundle.iova_cap)
        {
            return Err(KernelError::StaleCapability);
        }

        // Validate liveness/rights through the delegated driver's cspace.
        let driver_cnode = self.task_cnode(tid).ok_or(KernelError::TaskMissing)?;
        if self
            .capability_for_cnode(driver_cnode, bundle.irq_cap)
            .is_none()
            || self
                .capability_for_cnode(driver_cnode, bundle.dma_cap)
                .is_none()
            || self
                .capability_for_cnode(driver_cnode, bundle.iova_cap)
                .is_none()
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

        self.with_driver_state_mut(|driver| {
            let record = driver
                .driver_records
                .iter_mut()
                .flatten()
                .find(|record| record.tid == ThreadId(tid))
                .ok_or(KernelError::TaskMissing)?;
            record.dma_iova_base = Some(iova_base);
            record.dma_iova_len = Some(iova_len);
            Ok(())
        })
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
        let record = self.with_driver_state(|driver| {
            driver
                .driver_records
                .iter()
                .flatten()
                .find(|record| record.tid == ThreadId(tid))
                .copied()
        });
        let record = record.ok_or(KernelError::TaskMissing)?;

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
        self.with_driver_state_mut(|driver| {
            let record = driver
                .driver_records
                .iter_mut()
                .flatten()
                .find(|record| record.tid == ThreadId(tid))
                .ok_or(KernelError::TaskMissing)?;
            record.iova_space_cap = None;
            record.dma_iova_base = None;
            record.dma_iova_len = None;
            Ok(())
        })
    }

    pub fn revoke_driver_runtime_caps(&mut self, tid: u64) -> Result<(), KernelError> {
        let (irq_caps, dma_caps, iova_cap) = self.with_driver_state_mut(|driver| {
            let record = driver
                .driver_records
                .iter_mut()
                .flatten()
                .find(|record| record.tid == ThreadId(tid))
                .ok_or(KernelError::TaskMissing)?;

            let irq_caps = record.irq_caps;
            let dma_caps = record.dma_caps;
            record.irq_caps = [None; super::MAX_DRIVER_IRQ_CAPS];
            record.dma_caps = [None; super::MAX_DRIVER_DMA_CAPS];
            let iova_cap = record.iova_space_cap.take();
            record.dma_iova_base = None;
            record.dma_iova_len = None;
            Ok::<_, KernelError>((irq_caps, dma_caps, iova_cap))
        })?;
        let driver_cnode = self.task_cnode(tid).ok_or(KernelError::TaskMissing)?;

        for cap in irq_caps.into_iter().flatten() {
            let _ = self.revoke_capability_in_cnode(driver_cnode, cap);
        }
        for cap in dma_caps.into_iter().flatten() {
            let _ = self.revoke_capability_in_cnode(driver_cnode, cap);
        }
        if let Some(cap) = iova_cap {
            let _ = self.revoke_capability_in_cnode(driver_cnode, cap);
        }
        Ok(())
    }
}
