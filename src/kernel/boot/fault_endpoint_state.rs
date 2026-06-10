// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

impl KernelState {
    pub fn last_fault(&self) -> Option<FaultInfo> {
        self.with_fault_state(|faults| faults.last_fault)
    }

    pub fn clear_last_fault(&mut self) {
        self.with_fault_state_mut(|faults| {
            faults.last_fault = None;
            faults.last_fault_frame = None;
        });
    }

    pub fn record_fault(&mut self, fault: FaultInfo) {
        self.with_fault_state_mut(|faults| faults.last_fault = Some(fault));
    }

    pub fn record_fault_frame_snapshot(&mut self, frame: &TrapFrame) {
        self.with_fault_state_mut(|faults| faults.last_fault_frame = Some(frame.clone()));
    }

    pub fn last_fault_frame(&self) -> Option<TrapFrame> {
        self.with_fault_state(|faults| faults.last_fault_frame.clone())
    }

    pub fn set_fault_handler(&mut self, recv_cap: CapId) -> Result<(), KernelError> {
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        self.set_fault_handler_for_task(tid, recv_cap)
    }

    pub fn set_fault_handler_for_task(
        &mut self,
        tid: u64,
        recv_cap: CapId,
    ) -> Result<(), KernelError> {
        let cnode = self.task_cnode(tid).ok_or(KernelError::TaskMissing)?;
        let capability = self
            .capability_for_cnode(cnode, recv_cap)
            .ok_or(KernelError::InvalidCapability)?;
        if !capability.has_right(CapRights::RECEIVE) {
            return Err(KernelError::MissingRight);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;
        self.with_fault_state_mut(|faults| faults.fault_handler_endpoint = Some(endpoint_idx));
        Ok(())
    }

    pub fn set_fault_policy(&mut self, policy: FaultPolicy) {
        self.with_fault_state_mut(|faults| faults.fault_policy = policy);
    }

    pub fn fault_policy(&self) -> FaultPolicy {
        self.with_fault_state(|faults| faults.fault_policy)
    }

    pub fn set_task_fault_policy(
        &mut self,
        tid: u64,
        policy: Option<FaultPolicy>,
    ) -> Result<(), KernelError> {
        self.with_tcb_mut(tid, |tcb| {
            tcb.fault_policy_override = policy;
        })
        .ok_or(KernelError::TaskMissing)
    }

    pub(crate) fn effective_fault_policy_for(&self, tid: u64) -> FaultPolicy {
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .and_then(|tcb| tcb.fault_policy_override)
                .unwrap_or(self.with_fault_state(|faults| faults.fault_policy))
        })
    }

    pub fn task_asid(&self, tid: u64) -> Option<Asid> {
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .and_then(|tcb| tcb.asid)
        })
    }

    pub fn set_supervisor_endpoint(&mut self, recv_cap: CapId) -> Result<(), KernelError> {
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        self.set_supervisor_endpoint_for_task(tid, recv_cap)
    }

    pub fn set_supervisor_endpoint_for_task(
        &mut self,
        tid: u64,
        recv_cap: CapId,
    ) -> Result<(), KernelError> {
        let cnode = self.task_cnode(tid).ok_or(KernelError::TaskMissing)?;
        let capability = self
            .capability_for_cnode(cnode, recv_cap)
            .ok_or(KernelError::InvalidCapability)?;
        if !capability.has_right(CapRights::RECEIVE) {
            return Err(KernelError::MissingRight);
        }
        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;
        self.with_fault_state_mut(|faults| faults.supervisor_endpoint = Some(endpoint_idx));
        Ok(())
    }

    /// Stage 77+78: Register an endpoint for kernel→PM task-exit notifications.
    ///
    /// Mirrors `set_supervisor_endpoint_for_task`. The endpoint index is stored in
    /// `FaultSubsystem::pm_task_exit_endpoint` and used by `report_task_exit_to_pm`.
    pub fn set_pm_task_exit_endpoint_for_task(
        &mut self,
        tid: u64,
        recv_cap: CapId,
    ) -> Result<(), KernelError> {
        let cnode = self.task_cnode(tid).ok_or(KernelError::TaskMissing)?;
        let capability = self
            .capability_for_cnode(cnode, recv_cap)
            .ok_or(KernelError::InvalidCapability)?;
        if !capability.has_right(CapRights::RECEIVE) {
            return Err(KernelError::MissingRight);
        }
        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;
        self.with_fault_state_mut(|faults| faults.pm_task_exit_endpoint = Some(endpoint_idx));
        Ok(())
    }

    pub fn bind_task_asid(&mut self, tid: u64, asid: Asid) -> Result<(), KernelError> {
        if self.with_user_spaces(|spaces| spaces.get(asid).is_none()) {
            return Err(KernelError::Vm(VmError::InvalidAsid));
        }
        self.with_tcb_mut(tid, |tcb| {
            tcb.asid = Some(asid);
        })
        .ok_or(KernelError::TaskMissing)
    }

    pub fn unbind_task_asid(&mut self, tid: u64) -> Result<(), KernelError> {
        self.with_tcb_mut(tid, |tcb| {
            tcb.asid = None;
        })
        .ok_or(KernelError::TaskMissing)
    }
}
