// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState};
use crate::kernel::capabilities::CNodeId;
use crate::kernel::ipc::ThreadId;
use crate::kernel::task::{TaskClass, ThreadControlBlock};

impl KernelState {
    fn next_dynamic_tid_after(tid: u64) -> u64 {
        let next = tid.wrapping_add(1);
        if next < super::INITIAL_DYNAMIC_TID {
            super::INITIAL_DYNAMIC_TID
        } else {
            next
        }
    }

    pub(crate) fn register_task_with_class_in_process(
        &mut self,
        tid: u64,
        class: TaskClass,
        process_pid: u64,
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
        self.ensure_cnode_space(cnode)?;
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
        let mut candidate = self.next_dynamic_tid.max(super::INITIAL_DYNAMIC_TID);
        for _ in 0..=limits.max_tasks {
            if self.task_status(candidate).is_none() {
                self.next_dynamic_tid = Self::next_dynamic_tid_after(candidate);
                return Ok(candidate);
            }
            candidate = Self::next_dynamic_tid_after(candidate);
        }
        Err(KernelError::TaskTableFull)
    }

    pub fn task_class(&self, tid: u64) -> Option<TaskClass> {
        self.tcbs.iter().enumerate().find_map(|(idx, slot)| {
            slot.as_ref()
                .filter(|tcb| tcb.tid.0 == tid)
                .and(self.task_classes[idx])
        })
    }
}
