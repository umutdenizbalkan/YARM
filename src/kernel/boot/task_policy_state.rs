// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState, RuntimeCapacityConfig, TidAllocationTelemetry};
use crate::kernel::capabilities::CNodeId;
use crate::kernel::ipc::ThreadId;
use crate::kernel::task::{TaskClass, ThreadControlBlock};

impl KernelState {
    pub(crate) fn register_task_with_class_and_cnode_slots_in_process(
        &mut self,
        tid: u64,
        class: TaskClass,
        process_pid: u64,
        requested_cnode_slots: Option<usize>,
    ) -> Result<(), KernelError> {
        if self.task_status(tid).is_some() {
            return Ok(());
        }
        let limits = self.runtime_capacity_config();
        if self.with_tcbs(|tcbs| tcbs.iter().flatten().count()) >= limits.max_tasks {
            return Err(KernelError::TaskTableFull);
        }
        let cnode = self
            .process_cnode_for_pid(process_pid)
            .unwrap_or(CNodeId(process_pid));
        let cnode_slots =
            Self::requested_cnode_slot_capacity_for_class(class, limits, requested_cnode_slots)?;
        self.ensure_cnode_space_with_slots(cnode, cnode_slots)?;
        self.set_process_cnode_for_pid(process_pid, cnode)?;
        let inserted = if let Some(idx) = self.tcbs.iter().position(|slot| slot.is_none()) {
            let tcb = ThreadControlBlock::new(ThreadId(tid), None);
            self.tcbs[idx] = Some(tcb);
            self.task_classes[idx] = Some(class);
            true
        } else {
            false
        };
        if !inserted {
            return Err(KernelError::TaskTableFull);
        }
        self.provision_default_kernel_context(tid)?;
        Ok(())
    }

    pub(crate) fn register_task_with_class_in_process(
        &mut self,
        tid: u64,
        class: TaskClass,
        process_pid: u64,
    ) -> Result<(), KernelError> {
        self.register_task_with_class_and_cnode_slots_in_process(tid, class, process_pid, None)
    }

    pub fn register_task_with_class(
        &mut self,
        tid: u64,
        class: TaskClass,
    ) -> Result<(), KernelError> {
        self.register_task_with_class_in_process(tid, class, tid)
    }

    pub fn register_task(&mut self, tid: u64) -> Result<(), KernelError> {
        self.register_task_with_class(tid, TaskClass::App)
    }

    pub fn allocate_thread_id(&mut self) -> Result<u64, KernelError> {
        let limits = self.runtime_capacity_config();
        if self.with_tcbs(|tcbs| tcbs.iter().flatten().count()) >= limits.max_tasks {
            return Err(KernelError::TaskTableFull);
        }
        let policy = self.tid_allocation_policy;
        let raw_cursor = self.tid_allocation_cursor.raw_next_dynamic_tid();
        let mut candidate = self.tid_allocation_cursor.next_dynamic_tid(policy);
        if raw_cursor < policy.dynamic_tid_floor() {
            self.with_telemetry_state_mut(|telemetry| {
                telemetry.tid_allocation.gap_floor_repairs =
                    telemetry.tid_allocation.gap_floor_repairs.saturating_add(1);
            });
        }
        for _ in 0..=limits.max_tasks {
            debug_assert!(candidate > policy.static_tid_upper_bound());
            if self.task_status(candidate).is_none() {
                let wraps = policy.advance_dynamic_cursor(candidate) == policy.dynamic_tid_floor();
                self.tid_allocation_cursor
                    .advance_after_allocation(policy, candidate);
                self.with_telemetry_state_mut(|telemetry| {
                    telemetry.tid_allocation.dynamic_tid_allocations = telemetry
                        .tid_allocation
                        .dynamic_tid_allocations
                        .saturating_add(1);
                    if wraps {
                        telemetry.tid_allocation.dynamic_tid_wraps =
                            telemetry.tid_allocation.dynamic_tid_wraps.saturating_add(1);
                    }
                });
                if wraps {
                    crate::yarm_log!(
                        "YARM_TID_ALLOC_WRAP allocated={} reset_cursor_to={}",
                        candidate,
                        policy.dynamic_tid_floor()
                    );
                }
                return Ok(candidate);
            }
            candidate = policy.advance_dynamic_cursor(candidate);
        }
        Err(KernelError::TaskTableFull)
    }

    #[cfg(test)]
    pub(crate) fn set_dynamic_tid_cursor_for_test(&mut self, next_dynamic_tid: u64) {
        self.tid_allocation_cursor
            .set_next_dynamic_tid_for_test(next_dynamic_tid);
    }

    #[cfg(test)]
    pub(crate) fn next_dynamic_tid_for_test(&self) -> u64 {
        self.tid_allocation_cursor
            .next_dynamic_tid(self.tid_allocation_policy)
    }

    pub fn task_class(&self, tid: u64) -> Option<TaskClass> {
        self.tcbs.iter().enumerate().find_map(|(idx, slot)| {
            slot.as_ref()
                .filter(|tcb| tcb.tid.0 == tid)
                .and(self.task_classes[idx])
        })
    }

    pub fn tid_allocation_telemetry(&self) -> TidAllocationTelemetry {
        self.with_telemetry_state(|telemetry| telemetry.tid_allocation)
    }

    pub fn dynamic_tid_floor(&self) -> u64 {
        self.tid_allocation_policy.dynamic_tid_floor()
    }

    pub fn static_tid_upper_bound(&self) -> u64 {
        self.tid_allocation_policy.static_tid_upper_bound()
    }

    pub fn is_dynamic_tid(&self, tid: u64) -> bool {
        tid >= self.dynamic_tid_floor()
    }

    fn default_cnode_slot_capacity_for_class(
        class: TaskClass,
        limits: RuntimeCapacityConfig,
    ) -> usize {
        match class {
            TaskClass::Driver => limits.driver_cnode_slot_capacity,
            TaskClass::App | TaskClass::SystemServer => limits.default_cnode_slot_capacity,
        }
    }

    fn requested_cnode_slot_capacity_for_class(
        class: TaskClass,
        limits: RuntimeCapacityConfig,
        requested: Option<usize>,
    ) -> Result<usize, KernelError> {
        let default = Self::default_cnode_slot_capacity_for_class(class, limits);
        let requested = requested.unwrap_or(default);
        if requested == 0 {
            return Err(KernelError::WrongObject);
        }
        match class {
            TaskClass::App => {
                if requested != default {
                    return Err(KernelError::MissingRight);
                }
            }
            TaskClass::SystemServer | TaskClass::Driver => {}
        }
        Ok(requested)
    }
}
